#!/usr/bin/env python3
"""A reference X25519, RFC 7748 section 5, in plain Python integers.

The host peers' half of a keyed cluster session (step 5 of
docs/roadmap/roadmap-session-auth.md). It follows the RFC's own pseudocode
line for line, with Python's big integers for the field, so it shares no
structure with the Rust in `ed25519/src/x25519.rs` (five 51-bit limbs) that it
is the foreign observer for.

THE REFERENCE CHECKS ITSELF ON LOAD, as gen-sign-vectors.py does: importing it
asserts section 5.2's two single-call vectors, its one-iteration vector, and
section 6.1's Diffie-Hellman exchange. If an assertion fires, the reference is
wrong and nothing keyed with it should be trusted.

No constant-time anything: this runs on the host, against a test peer.
"""

P = 2**255 - 19
A24 = 121665
BASE = (9).to_bytes(32, "little")


def _decode_scalar(k):
    b = bytearray(k)
    b[0] &= 248
    b[31] &= 127
    b[31] |= 64
    return int.from_bytes(b, "little")


def _decode_u(u):
    b = bytearray(u)
    b[31] &= 127
    return int.from_bytes(b, "little")


def x25519(k, u):
    """The RFC's X25519(k, u), for 32-byte k and u. Returns 32 bytes, which is
    all zero for a small-order u; see `shared` for the refusing form."""
    k = _decode_scalar(k)
    x_1 = _decode_u(u) % P
    x_2, z_2, x_3, z_3 = 1, 0, x_1, 1
    swap = 0
    for t in range(254, -1, -1):
        k_t = (k >> t) & 1
        swap ^= k_t
        if swap:
            x_2, x_3 = x_3, x_2
            z_2, z_3 = z_3, z_2
        swap = k_t
        a = (x_2 + z_2) % P
        aa = a * a % P
        b = (x_2 - z_2) % P
        bb = b * b % P
        e = (aa - bb) % P
        c = (x_3 + z_3) % P
        d = (x_3 - z_3) % P
        da = d * a % P
        cb = c * b % P
        x_3 = (da + cb) ** 2 % P
        z_3 = x_1 * (da - cb) ** 2 % P
        x_2 = aa * bb % P
        z_2 = e * (aa + A24 * e) % P
    if swap:
        x_2, x_3 = x_3, x_2
        z_2, z_3 = z_3, z_2
    return (x_2 * pow(z_2, P - 2, P) % P).to_bytes(32, "little")


def public(k):
    """X25519(k, 9)."""
    return x25519(k, BASE)


def shared(k, peer):
    """X25519(k, peer), or None when it is all zero (a small-order peer key),
    which a keyed session must refuse (RFC 7748 section 6.1)."""
    s = x25519(k, peer)
    return None if s == bytes(32) else s


def _h(s):
    return bytes.fromhex(s)


assert x25519(_h("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4"),
              _h("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c")) \
    == _h("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552"), \
    "reference fails RFC 7748 5.2 vector 1"
assert x25519(_h("4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d"),
              _h("e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493")) \
    == _h("95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957"), \
    "reference fails RFC 7748 5.2 vector 2"
assert x25519(BASE, BASE) == _h("422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079"), \
    "reference fails RFC 7748 5.2 one iteration"
_A = _h("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")
_B = _h("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb")
assert public(_A) == _h("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"), \
    "reference fails RFC 7748 6.1 (Alice's public key)"
assert public(_B) == _h("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f"), \
    "reference fails RFC 7748 6.1 (Bob's public key)"
assert shared(_A, public(_B)) == shared(_B, public(_A)) \
    == _h("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742"), \
    "reference fails RFC 7748 6.1 (the shared secret)"
assert shared(_A, bytes(32)) is None, "reference accepts a small-order peer key"
