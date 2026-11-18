"""Exact checker for bilinear matrix-multiplication schemes.

A scheme of rank r is r triples (u, v, w) of integer/rational vectors: m_k = (u_k . vec(A)) * (v_k . vec(B)),
vec(C) = sum_k w_k m_k. It is valid iff sum_k u_k (x) v_k (x) w_k equals the n x n matmul tensor, checked exactly
with Fractions. No floats, no tolerance. Prints {"verdict": "pass"|"fail", "score": rank, "reason": ...}.

    python check.py problem.json scheme.json
"""
import json
import math
import re
import sys
from fractions import Fraction

MAX_PRODUCTS = 512
MAX_COEFF_STR = 64
MAX_COEFF_BITS = 256
_RAT_RE = re.compile(r"^\s*[+-]?\d+(?:/[+-]?\d+|\.\d+)?\s*$")


def parse_rational(x) -> Fraction:
    if isinstance(x, bool):
        raise ValueError("boolean is not a rational coefficient")
    if isinstance(x, int):
        if x.bit_length() > MAX_COEFF_BITS:
            raise ValueError("integer coefficient exceeds 256 bits")
        return Fraction(x)
    if isinstance(x, float):
        if not math.isfinite(x) or abs(x) >= (1 << MAX_COEFF_BITS):
            raise ValueError("non-finite or oversized float coefficient")
        f = Fraction(x)
    elif isinstance(x, str):
        if len(x) > MAX_COEFF_STR or not _RAT_RE.match(x):
            raise ValueError(f"invalid or oversized rational string: {x[:24]!r}")
        f = Fraction(x)
    else:
        raise ValueError(f"unsupported coefficient type: {type(x).__name__}")
    if f.numerator.bit_length() > MAX_COEFF_BITS or f.denominator.bit_length() > MAX_COEFF_BITS:
        raise ValueError("rational numerator/denominator exceeds 256 bits")
    return f


def matmul_tensor(n):
    d = n * n
    T = {}
    for i in range(n):
        for j in range(n):
            for k in range(n):
                T[(i * n + j, j * n + k, i * n + k)] = 1  # a_ij * b_jk -> c_ik
    return T, d


def check(problem, scheme):
    if not isinstance(scheme, dict):
        return {"verdict": "fail", "reason": "scheme must be a JSON object"}
    products = scheme.get("products")
    if not isinstance(products, list) or not (1 <= len(products) <= MAX_PRODUCTS):
        return {"verdict": "fail", "reason": f"products must be a list of length 1..{MAX_PRODUCTS}"}
    n = problem["n"]
    T, d = matmul_tensor(n)
    acc = {}
    for t in products:
        if not isinstance(t, dict) or not all(isinstance(t.get(key), list) for key in ("u", "v", "w")):
            return {"verdict": "fail", "reason": "each product must be an object with u, v, w lists"}
        if not (len(t["u"]) == len(t["v"]) == len(t["w"]) == d):
            return {"verdict": "fail", "reason": f"vectors must have length {d}"}
        try:
            u, v, w = ([parse_rational(x) for x in t[key]] for key in ("u", "v", "w"))
        except (ValueError, ZeroDivisionError) as e:
            return {"verdict": "fail", "reason": str(e)}
        for a, ua in enumerate(u):
            if ua == 0:
                continue
            for b, vb in enumerate(v):
                if vb == 0:
                    continue
                for c, wc in enumerate(w):
                    if wc:
                        acc[(a, b, c)] = acc.get((a, b, c), 0) + ua * vb * wc
    for key in set(acc) | set(T):
        if acc.get(key, 0) != T.get(key, 0):
            return {"verdict": "fail", "reason": f"tensor mismatch at {key}"}
    return {"verdict": "pass", "score": len(products)}


if __name__ == "__main__":
    problem = json.load(open(sys.argv[1]))
    scheme = json.load(open(sys.argv[2]))
    print(json.dumps(check(problem, scheme)))
