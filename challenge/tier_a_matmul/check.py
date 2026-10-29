"""Exact checker for bilinear matrix-multiplication schemes.

A scheme of rank r is r triples (u, v, w) of integer/rational vectors: m_k = (u_k . vec(A)) * (v_k . vec(B)),
vec(C) = sum_k w_k m_k. It is valid iff sum_k u_k (x) v_k (x) w_k equals the n x n matmul tensor, checked exactly
with Fractions. No floats, no tolerance. Prints {"verdict": "pass"|"fail", "score": rank, "reason": ...}.

    python check.py problem.json scheme.json
"""
import json
import sys
from fractions import Fraction


def matmul_tensor(n):
    d = n * n
    T = {}
    for i in range(n):
        for j in range(n):
            for k in range(n):
                T[(i * n + j, j * n + k, i * n + k)] = 1  # a_ij * b_jk -> c_ik
    return T, d


def check(problem, scheme):
    n = problem["n"]
    T, d = matmul_tensor(n)
    acc = {}
    for t in scheme["products"]:
        u, v, w = ([Fraction(x) for x in t[key]] for key in ("u", "v", "w"))
        if not (len(u) == len(v) == len(w) == d):
            return {"verdict": "fail", "reason": f"vectors must have length {d}"}
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
    return {"verdict": "pass", "score": len(scheme["products"])}


if __name__ == "__main__":
    problem = json.load(open(sys.argv[1]))
    scheme = json.load(open(sys.argv[2]))
    print(json.dumps(check(problem, scheme)))
