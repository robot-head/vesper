#!/usr/bin/env python3
"""Negative transport peer only; never evaluates or attests a model result."""

import struct
import sys
import time

READY = b'[1,"ready","vesper.resource-model42/1","4.24.0","797c613eb9b6d4ec95db23e3e00af9ac6657f24b",[4194304,262144,8,1000000,131072,2048,65,256,9999999999,20,4096,67108864]]'


def frame(output, payload):
    output.write(struct.pack("<I", len(payload)))
    output.write(payload)
    output.flush()


def read_exact(input_stream, length):
    result = bytearray()
    while len(result) < length:
        chunk = input_stream.read(length - len(result))
        if not chunk:
            raise EOFError("incomplete negative-peer request")
        result.extend(chunk)
    return result


def request():
    input_stream = sys.stdin.buffer
    length = struct.unpack("<I", read_exact(input_stream, 4))[0]
    if length > 4_194_304:
        raise ValueError("oversized negative-peer request")
    read_exact(input_stream, length)


def main():
    kind = sys.argv[1]
    output = sys.stdout.buffer
    if kind == "wrong_ready":
        frame(output, b'[1,"ready","wrong","4.24.0","797c613eb9b6d4ec95db23e3e00af9ac6657f24b",[4194304,262144,8,1000000,131072,2048,65,256,9999999999,20,4096,67108864]]')
        return
    if kind == "stdout_noise":
        output.write(b"noise")
        output.flush()
        return
    frame(output, READY)
    if kind == "blocked_stdin":
        time.sleep(10)
        return
    if kind in ("bad_bye", "trailing", "exit_failure", "no_bye", "bye_only"):
        if sys.stdin.buffer.read(1):
            raise ValueError("unexpected negative-peer request")
        if kind == "no_bye":
            return
        if kind == "bad_bye":
            frame(output, b'[1,"bye",1,[1,1,0,0,0,0,0]]')
        else:
            frame(output, b'[1,"bye",0,[0,0,0,0,0,0,0]]')
        if kind == "trailing":
            output.write(b"x")
            output.flush()
        if kind == "exit_failure":
            sys.exit(9)
        return
    request()
    if kind == "malformed":
        frame(output, b"{}")
    elif kind == "unknown_constructor":
        frame(output, b'[1,"result",0,"validation_error",[["invented"]]]')
    elif kind == "reordered":
        frame(output, b'[1,"result",1,"validation_error",[["invalidLimits"]]]')
    elif kind == "oversize":
        output.write(struct.pack("<I", 262_145))
        output.flush()
    elif kind == "zero_length":
        output.write(struct.pack("<I", 0))
        output.flush()
    elif kind == "truncated_header":
        output.write(bytes([5, 0]))
        output.flush()
    elif kind == "truncated_payload":
        output.write(struct.pack("<I", 50))
        output.write(b"[1,")
        output.flush()
    elif kind == "flood":
        try:
            for _ in range(1024):
                sys.stderr.buffer.write(b"x" * 4096)
                sys.stderr.buffer.flush()
        except BrokenPipeError:
            pass
        time.sleep(10)
    elif kind == "stall":
        time.sleep(10)
    elif kind == "crash":
        sys.exit(7)
    else:
        raise ValueError("unknown negative transport fixture")


if __name__ == "__main__":
    main()
