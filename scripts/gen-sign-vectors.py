#!/usr/bin/env python3
"""Generate signing test vectors for the `ed25519` crate.

A reference Ed25519 in plain Python integers (RFC 8032's appendix shape),
extended with sign/verify. Naive throughout - affine arithmetic on big
integers, no extended coordinates, no constant-time anything - so it shares no
structure with the Rust it checks.

THE REFERENCE CHECKS ITSELF FIRST. Running this asserts that it reproduces RFC
8032 section 7.1's PUBLISHED signatures for TEST 1 and TEST 2 before emitting
anything. If that assertion ever fires, the reference is wrong and none of its
output should be trusted - which is the failure mode an unchecked second
implementation by the same author cannot detect.
"""
import hashlib

P = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493
D = -121665 * pow(121666, P - 2, P) % P


def inv(x):
    return pow(x, P - 2, P)


def add(p1, p2):
    x1, y1 = p1
    x2, y2 = p2
    k = D * x1 * x2 * y1 * y2 % P
    return ((x1 * y2 + x2 * y1) * inv(1 + k) % P, (y1 * y2 + x1 * x2) * inv(1 - k) % P)


def mul(pt, e):
    r = (0, 1)
    while e > 0:
        if e & 1:
            r = add(r, pt)
        pt = add(pt, pt)
        e >>= 1
    return r


def recover_x(y, sign):
    if y >= P:
        return None
    x2 = (y * y - 1) * inv(D * y * y + 1) % P
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (P + 3) // 8, P)
    if (x * x - x2) % P:
        x = x * pow(2, (P - 1) // 4, P) % P
    if (x * x - x2) % P:
        return None
    if (x & 1) != sign:
        x = P - x
    return x


BY = 4 * inv(5) % P
B = (recover_x(BY, 0), BY)


def enc(pt):
    return int.to_bytes(pt[1] | ((pt[0] & 1) << 255), 32, "little")


def secret_expand(sk):
    h = hashlib.sha512(sk).digest()
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8
    a |= 1 << 254
    return a, h[32:]


def public_key(sk):
    a, _ = secret_expand(sk)
    return enc(mul(B, a))


def sign(sk, msg):
    a, prefix = secret_expand(sk)
    A = enc(mul(B, a))
    r = int.from_bytes(hashlib.sha512(prefix + msg).digest(), "little") % L
    R = enc(mul(B, r))
    k = int.from_bytes(hashlib.sha512(R + A + msg).digest(), "little") % L
    s = (r + k * a) % L
    return R + int.to_bytes(s, 32, "little")


# --- self-check against PUBLISHED values, before emitting anything ----------
_RFC1_SK = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"
_RFC1_SIG = ("e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155"
             "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b")
_RFC2_SK = "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb"
_RFC2_SIG = ("92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da"
             "085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00")
assert sign(bytes.fromhex(_RFC1_SK), b"").hex() == _RFC1_SIG, "reference fails RFC 8032 TEST 1"
assert sign(bytes.fromhex(_RFC2_SK), bytes([0x72])).hex() == _RFC2_SIG, "reference fails RFC 8032 TEST 2"

if __name__ == "__main__":
    # RFC 8032 section 7.1, plus messages that exercise the SHA-512 block
    # boundary (the message is hashed twice, at different offsets each time).
    cases = [
        ("rfc_test1", _RFC1_SK, b""),
        ("rfc_test2", _RFC2_SK, bytes([0x72])),
        ("rfc_test3", "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7", bytes([0xAF, 0x82])),
        ("rfc_test1024", "f5e5767cf153319517630f226876b86c8160cc583bc013744c6bf255f5cc0ee5",
         bytes.fromhex("08b8b2b733424243760fe426a4b54908632110a66c2f6591eabd3345e3e4eb98"
                       "fa6e264bf09efe12ee50f8f54e9f77b1e355f6c50544e23fb1433ddf73be84d8"
                       "79de7c0046dc4996d9e773f4bc9efe5738829adb26c81b37c93a1b270b20329d")),
        ("empty_again", "833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42", b""),
        ("one_byte_zero", _RFC1_SK, bytes([0x00])),
        ("len_31", _RFC1_SK, bytes(range(31))),
        ("len_32", _RFC1_SK, bytes(range(32))),
        ("len_63", _RFC2_SK, bytes(range(63))),
        ("len_64_block", _RFC2_SK, bytes(range(64))),
        ("len_65", _RFC2_SK, bytes(range(65))),
        ("len_128_two_blocks", _RFC2_SK, bytes([i % 251 for i in range(128)])),
        ("len_200", _RFC1_SK, bytes([(i * 7) % 256 for i in range(200)])),
    ]
    print("// (name, secret key, public key, message, signature)")
    for name, sk_hex, msg in cases:
        sk = bytes.fromhex(sk_hex)
        print(f'    ("{name}", "{sk_hex}", "{public_key(sk).hex()}", "{msg.hex()}", "{sign(sk, msg).hex()}"),')
