#!/usr/bin/env python3
"""Generate curve test vectors for the `ed25519` crate: a reference Ed25519 in
plain Python integers, after RFC 8032's appendix.

Deliberately the naive form - affine arithmetic on big integers, no extended
coordinates, no windowing, no constant-time anything. It shares no structure with
the Rust it checks, which is the whole reason it is usable as a check.

THIS REFERENCE IS ITSELF VERIFIED, which matters: an unchecked second
implementation by the same author is not a foreign observer, it is the same
assumptions typed twice. Running this script prints the base point as
5866666666666666666666666666666666666666666666666666666666666666 and the five
RFC 8032 section 7.1 public keys - all published values, none of them derived
from the Rust. If those lines ever change, the reference is wrong, not the code
under test.
"""

P = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493
D = -121665 * pow(121666, P - 2, P) % P

def inv(x): return pow(x, P - 2, P)

# Extended-free affine addition on the twisted Edwards curve -x^2+y^2 = 1+d x^2 y^2
def point_add(pt1, pt2):
    x1, y1 = pt1
    x2, y2 = pt2
    k = D * x1 * x2 * y1 * y2 % P
    x3 = (x1 * y2 + x2 * y1) * inv(1 + k) % P
    y3 = (y1 * y2 + x1 * x2) * inv(1 - k) % P
    return (x3, y3)

def scalar_mult(pt, e):
    result = (0, 1)   # the neutral element
    while e > 0:
        if e & 1:
            result = point_add(result, pt)
        pt = point_add(pt, pt)
        e >>= 1
    return result

def recover_x(y, sign):
    if y >= P: return None
    x2 = (y*y - 1) * inv(D*y*y + 1) % P
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (P + 3) // 8, P)
    if (x*x - x2) % P != 0:
        x = x * pow(2, (P - 1) // 4, P) % P
    if (x*x - x2) % P != 0: return None
    if (x & 1) != sign: x = P - x
    return x

BY = 4 * inv(5) % P
BX = recover_x(BY, 0)
B = (BX, BY)

def point_encode(pt):
    x, y = pt
    return int.to_bytes(y | ((x & 1) << 255), 32, "little")

def point_decode(b):
    y = int.from_bytes(b, "little")
    sign = (y >> 255) & 1
    y &= (1 << 255) - 1
    x = recover_x(y, sign)
    return None if x is None else (x, y)

if __name__ == "__main__":
    import hashlib
    print("// base point B, encoded")
    print(f'    ("base", "{point_encode(B).hex()}"),')
    print("// small multiples of B: kB for k = 1..8, then some larger ones")
    # 2**255 and friends are here because MUTATION TESTING found the top scalar
    # bit untested: truncating the loop to 255 iterations broke nothing, since a
    # clamped Ed25519 scalar always has bit 255 clear and every other vector is
    # small. `mul` accepts arbitrary 32 bytes, so the bit it never exercised is
    # exactly the bit an attacker would supply.
    for k in list(range(1, 9)) + [255, 256, 1000, 2**64, 2**252, L - 1, 2**255, 2**255 + 12345, 2**256 - 1]:
        if k < 2**32:
            label = f"mul_{k}"
        elif k == 2**64:
            label = "mul_2_64"
        elif k == 2**252:
            label = "mul_2_252"
        elif k == 2**255:
            label = "mul_2_255_top_bit"
        elif k == 2**255 + 12345:
            label = "mul_top_bit_plus"
        elif k == 2**256 - 1:
            label = "mul_all_ones"
        else:
            label = "mul_L_minus_1"
        # The scalar is emitted as 32 little-endian bytes, not as an integer:
        # that is how a scalar actually crosses the API, and L-1 does not fit in
        # any Rust integer type anyway.
        k_bytes = k.to_bytes(32, "little").hex()
        print(f'    ("{label}", "{k_bytes}", "{point_encode(scalar_mult(B, k)).hex()}"),')
    print()
    print("// RFC 8032 section 7.1 secret keys -> public keys")
    rfc_secrets = [
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
        "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
        "f5e5767cf153319517630f226876b86c8160cc583bc013744c6bf255f5cc0ee5",
        "833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42",
    ]
    for sk_hex in rfc_secrets:
        h = hashlib.sha512(bytes.fromhex(sk_hex)).digest()
        a = int.from_bytes(h[:32], "little")
        a &= (1 << 254) - 8          # clear the low 3 bits
        a |= (1 << 254)              # set bit 254
        pk = point_encode(scalar_mult(B, a))
        print(f'    ("{sk_hex[:8]}", "{sk_hex}", "{pk.hex()}"),')
