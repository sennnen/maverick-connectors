"""Dependency-free Ed25519 (RFC 8032) — TEST fixture signing only.

A self-contained signer so the test fixtures can be regenerated on any machine with nothing but a
Python interpreter. This is the public-domain reference construction; it is slow (a few signatures)
and is never on any production path. `tools/testsign.py` machine-verifies every signature it produces
with the Rust `ed25519-dalek` verifier, so a mistake here cannot slip a bad signature into a fixture.
"""

import hashlib

_Q = 2**255 - 19
_L = 2**252 + 27742317777372353535851937790883648493
_D = (-121665 * pow(121666, _Q - 2, _Q)) % _Q
_I = pow(2, (_Q - 1) // 4, _Q)


def _recover_x(y: int) -> int:
    xx = (y * y - 1) * pow(_D * y * y + 1, _Q - 2, _Q)
    x = pow(xx, (_Q + 3) // 8, _Q)
    if (x * x - xx) % _Q != 0:
        x = (x * _I) % _Q
    if x % 2 != 0:
        x = _Q - x
    return x


_BY = (4 * pow(5, _Q - 2, _Q)) % _Q
_B = (_recover_x(_BY) % _Q, _BY)


def _add(p: tuple[int, int], q: tuple[int, int]) -> tuple[int, int]:
    x1, y1 = p
    x2, y2 = q
    denom = _D * x1 * x2 * y1 * y2
    x3 = (x1 * y2 + x2 * y1) * pow(1 + denom, _Q - 2, _Q)
    y3 = (y1 * y2 + x1 * x2) * pow(1 - denom, _Q - 2, _Q)
    return (x3 % _Q, y3 % _Q)


def _mul(p: tuple[int, int], e: int) -> tuple[int, int]:
    result = (0, 1)
    while e > 0:
        if e & 1:
            result = _add(result, p)
        p = _add(p, p)
        e >>= 1
    return result


def _encode_int(y: int) -> bytes:
    return y.to_bytes(32, "little")


def _encode_point(p: tuple[int, int]) -> bytes:
    x, y = p
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


def _scalar(h: bytes) -> int:
    a = 2 ** 254 + sum(2**i * ((h[i // 8] >> (i % 8)) & 1) for i in range(3, 254))
    return a


def _hint(data: bytes) -> int:
    return int.from_bytes(hashlib.sha512(data).digest(), "little") % _L


def public_key(seed: bytes) -> bytes:
    """32-byte seed -> 32-byte Ed25519 public key, matching SigningKey::from_bytes(seed)."""
    h = hashlib.sha512(seed).digest()
    return _encode_point(_mul(_B, _scalar(h)))


def sign(message: bytes, seed: bytes) -> bytes:
    """64-byte Ed25519 signature over `message`, matching dalek's SigningKey::sign."""
    h = hashlib.sha512(seed).digest()
    a = _scalar(h)
    pk = _encode_point(_mul(_B, a))
    r = _hint(h[32:64] + message)
    big_r = _mul(_B, r)
    s = (r + _hint(_encode_point(big_r) + pk + message) * a) % _L
    return _encode_point(big_r) + _encode_int(s)
