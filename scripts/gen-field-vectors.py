#!/usr/bin/env python3
"""Generate field-arithmetic test vectors for the `ed25519` crate, mod 2^255-19.

Python's arbitrary-precision integers share NOTHING with the 5x51-bit limb
representation the Rust side uses - no limbs, no carries, no reduction chain -
which is exactly why they make a usable foreign observer for it. A vector this
script produces cannot be wrong in the same way the Rust can.
"""
import random

P = 2**255 - 19

def enc(x):
    """Canonical 32-byte little-endian encoding, as Ed25519 encodes field elements."""
    return (x % P).to_bytes(32, "little").hex()

# Fixed cases first: the boundaries where a limb-based implementation goes wrong.
fixed = [
    ("zero", 0),
    ("one", 1),
    ("two", 2),
    ("p_minus_1", P - 1),
    ("p_minus_2", P - 2),
    ("limb_boundary_2_51", 2**51),          # exactly one limb
    ("limb_boundary_2_51_minus_1", 2**51 - 1),
    ("limb_boundary_2_102", 2**102),        # two limbs
    ("two_204", 2**204),                    # four limbs
    ("just_under_2_255", 2**255 - 20),      # = P - 1, the largest canonical value
    ("high_bit_pattern", 0x7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff),
    ("alternating", int("55" * 32, 16) % P),
    ("alternating_aa", int("aa" * 32, 16) % P),
]

rng = random.Random(20260831)   # fixed seed: the vectors are reproducible
randoms = [(f"rand_{i}", rng.randrange(0, P)) for i in range(8)]
elements = fixed + randoms

print("// --- single-element operations: (name, input, square, invert) ---")
for name, a in elements:
    inv = pow(a, P - 2, P) if a % P != 0 else 0   # 0 has no inverse; ref impls return 0
    print(f'    ("{name}", "{enc(a)}", "{enc(a*a)}", "{enc(inv)}"),')

print()
print("// --- pair operations: (name, a, b, a+b, a-b, a*b) ---")
pairs = [
    ("one_one", 1, 1),
    ("p_minus_1_plus_1", P - 1, 1),          # wraps to zero: the reduction case
    ("p_minus_1_times_2", P - 1, 2),
    ("zero_minus_one", 0, 1),                # borrows: the underflow case
    ("big_times_big", P - 2, P - 3),         # the largest products
    ("limb_carry", 2**51 - 1, 2**51 - 1),    # carry out of one limb into the next
    ("high_times_high", 2**204, 2**204),     # product overflows 255 bits hard
]
for i in range(6):
    pairs.append((f"rand_pair_{i}", rng.randrange(0, P), rng.randrange(0, P)))
for name, a, b in pairs:
    print(f'    ("{name}", "{enc(a)}", "{enc(b)}", "{enc(a+b)}", "{enc(a-b)}", "{enc(a*b)}"),')
