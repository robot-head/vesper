#!/usr/bin/env python3
"""Regenerate crmath_refs.txt: correctly rounded references from mpmath.

Each function is evaluated with mpmath at 400 and at 700 bits; both are
rounded (exactly, with Python integers) to the target format and must agree.
Run: python3 -I gen_crmath_refs.py > crmath_refs.txt   (needs mpmath)
"""
import random
import struct
from fractions import Fraction

import mpmath

import os
R = random.Random(int(os.environ.get("SEED", "0x5E5BE2"), 0))


def f64_bits(x):
    return struct.unpack('<Q', struct.pack('<d', x))[0]


def f32_bits(x):
    return struct.unpack('<I', struct.pack('<f', x))[0]


def bits_to_f64(b):
    return struct.unpack('<d', struct.pack('<Q', b))[0]


def bits_to_f32(b):
    return struct.unpack('<f', struct.pack('<I', b))[0]


def round_fraction(q, p, emin, emax):
    """Bits of the correctly rounded (nearest-even) value of Fraction q."""
    sign = 1 if q < 0 else 0
    q = abs(q)
    frac_bits = p - 1
    exp_bits = emax.bit_length() + 1
    sign_bit = sign << (frac_bits + exp_bits)
    if q == 0:
        return sign_bit
    # exponent of leading bit
    e = q.numerator.bit_length() - q.denominator.bit_length()
    while Fraction(2) ** e > q:
        e -= 1
    while Fraction(2) ** (e + 1) <= q:
        e += 1
    inf = ((1 << exp_bits) - 1) << frac_bits
    if e > emax:
        return sign_bit | inf
    qexp = max(e, emin) - (p - 1)
    scaled = q / (Fraction(2) ** qexp)
    m = scaled.numerator // scaled.denominator
    rem = scaled - m
    if rem > Fraction(1, 2) or (rem == Fraction(1, 2) and m % 2 == 1):
        m += 1
    if m.bit_length() > p:
        m >>= 1
        qexp += 1
    if m == 0:
        return sign_bit
    if m.bit_length() - 1 + qexp > emax:
        return sign_bit | inf
    if m.bit_length() < p:
        return sign_bit | m
    biased = qexp + p - 1 + emax
    return sign_bit | (biased << frac_bits) | (m & ((1 << frac_bits) - 1))


FORMATS = {
    'f64': (53, -1022, 1023),
    'f32': (24, -126, 127),
}


def to_fraction(v):
    sign, man, exp, _ = v._mpf_
    q = Fraction(int(man)) * (Fraction(2) ** int(exp))
    return -q if sign else q


def evaluate(name, args, fmt):
    out = []
    for prec in (400, 700):
        mpmath.mp.prec = prec
        a = [mpmath.mpf(x) for x in args]
        if name == 'sin':
            v = mpmath.sin(a[0])
        elif name == 'cos':
            v = mpmath.cos(a[0])
        elif name == 'exp':
            v = mpmath.exp(a[0])
        elif name == 'log':
            v = mpmath.log(a[0])
        elif name == 'atan2':
            v = mpmath.atan2(a[0], a[1])
        out.append(round_fraction(to_fraction(v), *FORMATS[fmt]))
    assert out[0] == out[1], (name, args, fmt)
    return out[0]


def rand_f64_bits(lo_exp=-1074, hi_exp=1023, positive=False):
    e = R.randint(lo_exp, hi_exp)
    m = R.getrandbits(52) | (1 << 52)
    x = float(Fraction(m, 1 << 52) * (Fraction(2) ** e))
    if not positive and R.random() < 0.5:
        x = -x
    return x


def as_f32(x):
    if abs(x) > 3.4e38:
        return float('nan')
    return bits_to_f32(f32_bits(x))


def cases():
    n = int(os.environ.get("N", "300"))
    exp_in = [R.uniform(-745, 709) for _ in range(n)] + [R.uniform(-1, 1) for _ in range(n // 3)]
    exp_in += [rand_f64_bits(-80, -1) for _ in range(n // 3)]
    exp_in += [709.78, -745.1, -708.5, 1e-300, -1e-300, 0.5, 1.0, -1.0, 2.0 ** -53]
    log_in = [abs(rand_f64_bits()) for _ in range(n)]
    log_in += [abs(rand_f64_bits(-149, 127)) for _ in range(n)]
    log_in += [1.0 + R.uniform(-1e-3, 1e-3) for _ in range(n // 3)]
    log_in += [1.0 + 2.0 ** -52, 1.0 - 2.0 ** -53, 5e-324, 1.7976931348623157e308, 2.0, 0.5, 10.0]
    trig_in = [R.uniform(-10, 10) for _ in range(n)]
    trig_in += [rand_f64_bits(-60, 200) for _ in range(n // 2)]
    trig_in += [rand_f64_bits(-1074, 1023) for _ in range(n // 3)]
    trig_in += [1e22, 2.0 ** 1023, 355.0, 103993.0, 1.5707963267948966,
                3.141592653589793, float(6381956970095103 * 2 ** 797), 1e-310]
    atan_in = [(R.uniform(-10, 10), R.uniform(-10, 10)) for _ in range(n)]
    atan_in += [(rand_f64_bits(), rand_f64_bits()) for _ in range(n // 2)]
    atan_in += [(1.0, 1.0), (-1.0, -1.0), (1e-300, 1e300), (1e300, -1e-300), (3.0, -4.0)]
    for fmt in ('f64', 'f32'):
        conv = (lambda x: x) if fmt == 'f64' else as_f32
        bits = f64_bits if fmt == 'f64' else f32_bits
        for name, inputs in (('exp', exp_in), ('log', log_in), ('sin', trig_in), ('cos', trig_in)):
            seen = set()
            for x in inputs:
                x = conv(x)
                if x != x or x in (float('inf'), float('-inf')) or x == 0:
                    continue
                if name == 'log' and x <= 0:
                    continue
                if name == 'exp' and fmt == 'f32' and abs(x) > 150:
                    x = x / 8
                    x = conv(x)
                if bits(x) in seen:
                    continue
                seen.add(bits(x))
                r = evaluate(name, [x], fmt)
                print(f'{name}{"" if fmt == "f64" else "f"} {bits(x):x} {r:x}')
        seen = set()
        for y, x in atan_in:
            y, x = conv(y), conv(x)
            if y == 0 or x == 0 or y != y or x != x or abs(y) == float('inf') or abs(x) == float('inf'):
                continue
            key = (bits(y), bits(x))
            if key in seen:
                continue
            seen.add(key)
            r = evaluate('atan2', [y, x], fmt)
            print(f'atan2{"" if fmt == "f64" else "f"} {bits(y):x} {bits(x):x} {r:x}')


if __name__ == '__main__':
    cases()
