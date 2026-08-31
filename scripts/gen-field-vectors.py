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
    ("alternating", int("55" * 32, 16) % P),
]

# NON-CANONICAL inputs: raw 32-byte strings that are NOT the canonical encoding
# of the value they denote, paired with the value a decoder must produce.
#
# These are listed separately because emitting them through `enc()` would defeat
# them: `enc` reduces mod p first, so "2^255 - 1" came out as the two-byte value
# 18 and "just under 2^255" was byte-identical to p-1 - three rows that named
# boundaries they did not exercise. A decoder is the only thing that can see the
# difference, so these feed `decode` directly.
def decoded(raw):
    """What a conforming Ed25519 field decoder must produce for these bytes.

    Bit 255 is MASKED OFF FIRST and only then reduced - that ordering is the
    whole point, because Ed25519 stores the sign of x in that bit. Reducing the
    full 256-bit integer instead gives a different answer for any input with the
    top bit set (`all_ff` becomes 37 rather than 18), and the first version of
    this table did exactly that - vectors that would have failed a correct
    implementation.
    """
    return (int.from_bytes(raw, "little") & ((1 << 255) - 1)) % P

NONCANONICAL_RAW = [
    ("p_itself", (P).to_bytes(32, "little")),
    ("p_plus_1", (P + 1).to_bytes(32, "little")),
    ("two_255_minus_1", (2**255 - 1).to_bytes(32, "little")),
    ("high_bit_set_on_one", (1 | (1 << 255)).to_bytes(32, "little")),
    ("all_ff", bytes([0xFF] * 32)),
    ("alternating_aa", bytes([0xAA] * 32)),
    ("high_bit_on_p_minus_1", ((P - 1) | (1 << 255)).to_bytes(32, "little")),
]
NONCANONICAL = [(n, raw, decoded(raw)) for n, raw in NONCANONICAL_RAW]

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

print()
print("// --- non-canonical decode: (name, raw bytes, the value they must reduce to) ---")
for name, raw, want in NONCANONICAL:
    print(f'    ("{name}", "{raw.hex()}", "{enc(want)}"),')
