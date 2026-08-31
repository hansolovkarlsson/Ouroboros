#!/usr/bin/env python3
"""Generate scalar-arithmetic test vectors (mod L) for the `ed25519` crate.

Python's arbitrary-precision `%` shares nothing with the crate's bit-by-bit long
division, which is the point. `scalar.rs` was previously covered only INDIRECTLY,
through the signing vectors - so a reduction bug could only be seen as a wrong
signature, and only for the values a signature happens to produce.
"""
import random

L = 2**252 + 27742317777372353535851937790883648493


def enc(x, n=32):
    return (x % L).to_bytes(n, "little").hex()


def raw(x, n):
    return x.to_bytes(n, "little").hex()


rng = random.Random(20260831)

print("// --- reduce a 512-bit value mod L: (name, 64 raw bytes, result) ---")
wide_cases = [
    ("zero", 0),
    ("one", 1),
    ("l_minus_1", L - 1),
    ("l_itself", L),
    ("l_plus_1", L + 1),
    ("two_l_minus_1", 2 * L - 1),
    ("two_l", 2 * L),
    ("2_255", 2**255),
    ("2_256", 2**256),
    ("2_511", 2**511),
    ("all_ones_512", 2**512 - 1),
]
wide_cases += [(f"rand_{i}", rng.randrange(0, 2**512)) for i in range(8)]
for name, v in wide_cases:
    print(f'    ("{name}", "{raw(v, 64)}", "{enc(v)}"),')

print()
print("// --- (a*b + c) mod L: (name, a, b, c, result) ---")
pairs = [
    ("zeros", 0, 0, 0),
    ("one_one_zero", 1, 1, 0),
    ("max_max_max", L - 1, L - 1, L - 1),
    ("l_minus_1_times_2", L - 1, 2, 0),
    ("carry_chain", 2**252, 2**252, 2**252),
    ("add_wraps", 1, 1, L - 2),
    ("add_wraps_past", 1, 1, L - 1),
]
pairs += [(f"rand_{i}", rng.randrange(0, L), rng.randrange(0, L), rng.randrange(0, L)) for i in range(8)]
for name, a, b, c in pairs:
    print(f'    ("{name}", "{enc(a)}", "{enc(b)}", "{enc(c)}", "{enc(a * b + c)}"),')

print()
print("// --- canonicality: (name, 32 raw bytes, is_canonical) ---")
canon = [
    ("zero", 0, True),
    ("one", 1, True),
    ("l_minus_1", L - 1, True),
    ("l_itself", L, False),
    ("l_plus_1", L + 1, False),
    ("2_255_minus_1", 2**255 - 1, False),
    ("all_ones", 2**256 - 1, False),
    ("2_252", 2**252, True),
]
for name, v, ok in canon:
    print(f'    ("{name}", "{raw(v, 32)}", {str(ok).lower()}),')
