"""HiKVP: 3-level Hierarchical Key-Value Pruning (`Root -> Inter -> Leaf`) with Playbox trust gating.

Mirrors AtlasKV's hierarchical pruning structure (Root cluster -> Intermediate cluster -> Leaf triples)
so queries touch only a tiny sub-linear slice of a large repository or room memory, while enforcing
Playbox's provenance boundary (`quarantined`/`untrusted` triples are dropped before prompt injection).

Default scoring uses pure-stdlib identifier-aware TF-IDF cosine similarity (zero GPU, <20 ms on
Django-sized repos) plus optional 1-hop call-graph expansion; pass `embed_fn` to plug in dense
sentence embeddings (e.g. `all-MiniLM-L6-v2` matching AtlasKV's `KBEncoder`).
"""
from __future__ import annotations

import json
import math
import re
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Sequence

from .kg2kv import TRUSTED_TIERS, Triple

_CAMEL_SPLIT = re.compile(r"(?<=[a-z0-9])(?=[A-Z])|(?<=[A-Z])(?=[A-Z][a-z])")
_TOKEN_RE = re.compile(r"[A-Za-z0-9_]+")
_STOPWORDS = frozenset({
    "the", "and", "for", "with", "that", "this", "from", "into", "when", "does", "not",
    "are", "was", "were", "have", "has", "had", "but", "all", "can", "use", "self", "cls",
    "def", "class", "none", "true", "false", "return", "file", "line", "code", "bug",
})


def tokenize(text: str) -> list[str]:
    """Identifier-aware tokenizer: keeps full snake/camel tokens AND splits sub-identifiers."""
    out: list[str] = []
    for raw in _TOKEN_RE.findall(text or ""):
        low = raw.lower()
        if len(low) >= 2 and low not in _STOPWORDS:
            out.append(low)
        for part in raw.split("_"):
            for sub in _CAMEL_SPLIT.split(part):
                s = sub.lower()
                if len(s) >= 2 and s != low and s not in _STOPWORDS:
                    out.append(s)
    return out


SparseVec = dict[str, float]


def _normalize(vec: SparseVec) -> SparseVec:
    norm = math.sqrt(sum(v * v for v in vec.values()))
    if norm <= 0.0:
        return {}
    inv = 1.0 / norm
    return {k: v * inv for k, v in vec.items()}


def _dot(a: SparseVec, b: SparseVec) -> float:
    if len(a) > len(b):
        a, b = b, a
    return sum(v * b[k] for k, v in a.items() if k in b)


def _centroid(vecs: Sequence[SparseVec], max_terms: int = 128) -> SparseVec:
    acc: dict[str, float] = defaultdict(float)
    for v in vecs:
        for k, w in v.items():
            acc[k] += w
    if len(acc) > max_terms:
        top = sorted(acc.items(), key=lambda kv: kv[1], reverse=True)[:max_terms]
        acc = dict(top)
    return _normalize(acc)


@dataclass(frozen=True)
class QueryHit:
    triple: Triple
    score: float
    hop: int = 0   # 0 = direct HiKVP leaf hit, 1..k = k-hop structural call-graph / inheritance expansion
    via: str = ""  # optional provenance edge label (e.g. `watched_files -> iter_all_python_module_files`)


class HiKVPIndex:
    """Three-level hierarchical index (`Root -> Inter -> Leaf`) with multi-hop call-graph adjacency."""

    def __init__(
        self,
        triples: list[Triple],
        idf: dict[str, float],
        leaf_vecs: list[SparseVec],
        root_children: dict[str, list[str]],
        inter_children: dict[str, list[int]],
        root_centroids: dict[str, SparseVec],
        inter_centroids: dict[str, SparseVec],
        embed_fn: Callable[[str], Sequence[float]] | None = None,
    ):
        self.triples = triples
        self.idf = idf
        self.leaf_vecs = leaf_vecs
        self.root_children = root_children
        self.inter_children = inter_children
        self.root_centroids = root_centroids
        self.inter_centroids = inter_centroids
        self.embed_fn = embed_fn

        # Build fast symbol lookup for multi-hop call-graph & inheritance expansion.
        self._symbol_to_leaves: dict[str, list[int]] = defaultdict(list)
        self._callee_to_callers: dict[str, list[int]] = defaultdict(list)
        self._entity_to_calls: dict[str, list[int]] = defaultdict(list)
        self._entity_to_inherits: dict[str, list[int]] = defaultdict(list)
        self._class_to_methods: dict[str, list[int]] = defaultdict(list)
        for idx, t in enumerate(triples):
            short = t.entity.rsplit(".", 1)[-1].rsplit("::", 1)[-1]
            if t.relation in ("function", "method", "definition", "fn", "struct", "class"):
                self._symbol_to_leaves[short].append(idx)
                if t.relation == "method" and "." in t.entity:
                    owner = t.entity.rsplit(".", 1)[0]
                    self._class_to_methods[owner].append(idx)
            elif t.relation == "calls":
                self._entity_to_calls[t.entity].append(idx)
                for callee in (c.strip() for c in t.value.split(",") if c.strip()):
                    self._callee_to_callers[callee].append(idx)
            elif t.relation == "inherits":
                self._entity_to_inherits[t.entity].append(idx)

    def _resolve_symbol(self, name: str, preferred_file: str, *, limit: int = 2) -> list[int]:
        """Resolve a symbol short name, preferring definitions in the same file first."""
        cands = self._symbol_to_leaves.get(name, ())
        if not cands:
            return []
        same = [i for i in cands if self.triples[i].file == preferred_file]
        if same:
            return same[:limit]
        return list(cands[:1])

    @classmethod
    def build(
        cls,
        triples: Sequence[Triple],
        embed_fn: Callable[[str], Sequence[float]] | None = None,
    ) -> HiKVPIndex:
        tlist = list(triples)
        n = max(len(tlist), 1)
        df: Counter[str] = Counter()
        doc_tokens: list[list[str]] = []
        for t in tlist:
            toks = tokenize(f"{t.key_text} {t.value}")
            doc_tokens.append(toks)
            df.update(set(toks))
        idf = {tok: math.log((n + 1.0) / (c + 1.0)) + 1.0 for tok, c in df.items()}

        leaf_vecs: list[SparseVec] = []
        root_to_inters: dict[str, set[str]] = defaultdict(set)
        inter_children: dict[str, list[int]] = defaultdict(list)

        for idx, (t, toks) in enumerate(zip(tlist, doc_tokens)):
            tf = Counter(toks)
            # Weight entity & relation tokens higher (mirrors KG2KV key focus).
            ent_toks = set(tokenize(f"{t.entity} {t.relation}"))
            vec = {
                tok: (1.0 + math.log(cnt)) * idf.get(tok, 1.0) * (1.8 if tok in ent_toks else 1.0)
                for tok, cnt in tf.items()
            }
            leaf_vecs.append(_normalize(vec))
            root_to_inters[t.root_key].add(t.inter_key)
            inter_children[t.inter_key].append(idx)

        root_children = {r: sorted(inters) for r, inters in root_to_inters.items()}
        inter_centroids = {
            inter: _centroid([leaf_vecs[i] for i in idxs], max_terms=96)
            for inter, idxs in inter_children.items()
        }
        root_centroids = {
            root: _centroid([inter_centroids[i] for i in inters], max_terms=128)
            for root, inters in root_children.items()
        }
        return cls(
            triples=tlist,
            idf=idf,
            leaf_vecs=leaf_vecs,
            root_children=root_children,
            inter_children=dict(inter_children),
            root_centroids=root_centroids,
            inter_centroids=inter_centroids,
            embed_fn=embed_fn,
        )

    def _query_vec(self, text: str) -> SparseVec:
        tf = Counter(tokenize(text))
        vec = {tok: (1.0 + math.log(cnt)) * self.idf.get(tok, 1.0) for tok, cnt in tf.items() if tok in self.idf}
        return _normalize(vec)

    def query(
        self,
        text: str,
        *,
        top_root: int = 5,
        top_inter: int = 12,
        top_leaf: int = 14,
        expand_hops: int = 3,
        allow_untrusted: bool = False,
    ) -> list[QueryHit]:
        """Sub-linear 3-level HiKVP beam search with multi-hop call-chain expansion & trust gating."""
        qvec = self._query_vec(text)
        if not qvec or not self.triples:
            return []

        # Level 0: Root pruning
        root_scores = sorted(
            ((_dot(qvec, cvec), r) for r, cvec in self.root_centroids.items()),
            key=lambda x: x[0],
            reverse=True,
        )
        kept_roots = [r for sc, r in root_scores[:top_root] if sc > 0.0]
        for sc, r in root_scores[top_root:]:
            if sc > 0.0 and r.startswith("memory/"):
                kept_roots.append(r)
        if not kept_roots:
            return []

        # Level 1: Intermediate pruning (only children of kept roots)
        inter_scores: list[tuple[float, str]] = []
        for r in kept_roots:
            for inter in self.root_children.get(r, ()):
                sc = _dot(qvec, self.inter_centroids.get(inter, {}))
                if sc > 0.0:
                    inter_scores.append((sc, inter))
        inter_scores.sort(key=lambda x: x[0], reverse=True)
        kept_inters = [inter for _, inter in inter_scores[:top_inter]]
        for sc, inter in inter_scores[top_inter:]:
            if sc > 0.0 and inter.startswith("memory/"):
                kept_inters.append(inter)
        if not kept_inters:
            return []

        # Level 2: Leaf scoring + trust gate (prioritize symbol definitions & reviewed lessons over raw `calls` lists)
        leaf_scores: list[tuple[float, int]] = []
        for inter in kept_inters:
            for idx in self.inter_children.get(inter, ()):
                t = self.triples[idx]
                if not allow_untrusted and t.trust not in TRUSTED_TIERS:
                    continue
                sc = _dot(qvec, self.leaf_vecs[idx])
                if sc > 0.0:
                    if t.relation in ("calls", "imports", "inherits"):
                        sc *= 0.75
                    elif t.trust == "reviewed":
                        sc *= 1.35
                    leaf_scores.append((sc, idx))
        leaf_scores.sort(key=lambda x: x[0], reverse=True)

        hits: list[QueryHit] = []
        seen_idx: set[int] = set()
        for sc, idx in leaf_scores[:top_leaf]:
            seen_idx.add(idx)
            hits.append(QueryHit(triple=self.triples[idx], score=round(sc, 4), hop=0))

        # Multi-hop structural BFS: follows class inheritance (`inherits` -> base methods) and
        # call-graph edges (`function/method` -> `calls` -> callee definition) while enforcing trust tiers.
        if expand_hops > 0 and hits:
            frontier = list(hits)
            max_extra = max(18, top_leaf + 4)
            per_hop_cap = max(8, (max_extra + int(expand_hops) - 1) // max(int(expand_hops), 1))
            added: list[QueryHit] = []

            def _try_add(target_idx: int, score: float, hop: int, via: str, src_file: str, bucket: list[QueryHit]) -> None:
                if target_idx in seen_idx:
                    return
                cand = self.triples[target_idx]
                if not allow_untrusted and cand.trust not in TRUSTED_TIERS:
                    return
                seen_idx.add(target_idx)
                boost = 1.18 if cand.file == src_file else 0.72
                bucket.append(QueryHit(triple=cand, score=round(score * boost, 4), hop=hop, via=via))

            for hop in range(1, int(expand_hops) + 1):
                if len(added) >= max_extra or not frontier:
                    break
                decay = 0.82 ** hop
                next_frontier: list[QueryHit] = []
                for h in frontier:
                    t = h.triple
                    src_short = t.entity.rsplit(".", 1)[-1].rsplit("::", 1)[-1]
                    # 1) Direct `calls` triple -> resolve callees (same-file preferred)
                    if t.relation == "calls":
                        for callee in (c.strip() for c in t.value.split(",") if c.strip()):
                            for target_idx in self._resolve_symbol(callee, t.file):
                                _try_add(target_idx, h.score * decay, hop, f"{src_short} -> {callee}", t.file, next_frontier)
                    # 2) Function or method -> follow its own `calls` triples to callee definitions
                    elif t.relation in ("function", "method"):
                        for call_idx in self._entity_to_calls.get(t.entity, ()):
                            ct = self.triples[call_idx]
                            for callee in (c.strip() for c in ct.value.split(",") if c.strip()):
                                for target_idx in self._resolve_symbol(callee, t.file):
                                    _try_add(target_idx, h.score * decay, hop, f"{src_short} -> {callee}", t.file, next_frontier)
                        # On hop 1, also pull callers within the same file so entrypoints are visible.
                        if hop == 1:
                            for caller_idx in self._callee_to_callers.get(src_short, ()):
                                if self.triples[caller_idx].file == t.file:
                                    cshort = self.triples[caller_idx].entity.rsplit(".", 1)[-1]
                                    _try_add(caller_idx, h.score * decay * 0.85, hop, f"{cshort} -> {src_short}", t.file, next_frontier)
                                    if len(next_frontier) >= max_extra:
                                        break
                    # 3) Class definition -> follow base classes (`inherits`) & key inherited methods
                    elif t.relation in ("definition", "inherits"):
                        base_lists = [t.value] if t.relation == "inherits" else [
                            self.triples[i].value for i in self._entity_to_inherits.get(t.entity, ())
                        ]
                        for bval in base_lists:
                            for bname in (b.strip() for b in bval.split(",") if b.strip()):
                                for b_idx in self._resolve_symbol(bname, t.file, limit=1):
                                    bt = self.triples[b_idx]
                                    _try_add(b_idx, h.score * decay, hop, f"{src_short} inherits {bname}", t.file, next_frontier)
                                    m_idxs = [mi for mi in self._class_to_methods.get(bt.entity, ()) if not self.triples[mi].entity.endswith(".__init__")]
                                    scored_m = sorted(
                                        ((_dot(qvec, self.leaf_vecs[mi]) + (0.05 if self._entity_to_calls.get(self.triples[mi].entity) else 0.0), mi) for mi in m_idxs),
                                        key=lambda x: x[0],
                                        reverse=True,
                                    )
                                    for msc, mi in scored_m[:3]:
                                        mshort = self.triples[mi].entity.rsplit(".", 1)[-1]
                                        _try_add(mi, max(h.score * decay * 0.95, msc), hop, f"{bname}.{mshort}", t.file, next_frontier)
                next_frontier.sort(
                    key=lambda x: (
                        x.triple.relation in ("function", "method"),
                        "->" in x.via,
                        x.score,
                    ),
                    reverse=True,
                )
                budget_left = min(per_hop_cap, max_extra - len(added))
                kept = next_frontier[:budget_left]
                added.extend(kept)
                frontier = kept[:10]
            hits.extend(added)

        return hits

    def stats(self) -> dict:
        trusted = sum(1 for t in self.triples if t.trust in TRUSTED_TIERS)
        return {
            "roots": len(self.root_children),
            "inters": len(self.inter_children),
            "leaves": len(self.triples),
            "trusted_leaves": trusted,
            "quarantined_leaves": len(self.triples) - trusted,
        }

    def save(self, path: str | Path) -> None:
        payload = {
            "version": 1,
            "triples": [t.to_dict() for t in self.triples],
        }
        Path(path).write_text(json.dumps(payload), encoding="utf-8")

    @classmethod
    def load(cls, path: str | Path) -> HiKVPIndex:
        payload = json.loads(Path(path).read_text(encoding="utf-8"))
        triples = [Triple.from_dict(d) for d in payload.get("triples", [])]
        return cls.build(triples)


def format_brief(hits: Sequence[QueryHit], *, max_chars: int = 2800) -> str:
    """Format HiKVP hits into a compact, prefix-cache-friendly structural map with call chains."""
    if not hits:
        return ""
    lines = ["[Playbox AtlasKV structural brief — verified repo/room symbols]"]
    edges = [h.via for h in hits if h.via and "->" in h.via]
    if edges:
        uniq_edges = list(dict.fromkeys(edges))[:10]
        lines.append("Call edges: " + " ; ".join(uniq_edges))
    used = sum(len(x) + 1 for x in lines)
    # Render symbol definitions/lessons ahead of raw `calls`/`imports` triples so deep call-chain
    # definitions always stay within the character budget.
    ordered = sorted(
        hits,
        key=lambda h: (
            h.triple.trust != "reviewed",
            h.triple.relation in ("calls", "imports", "inherits"),
            h.hop,
            -h.score,
        ),
    )
    for h in ordered:
        tag = f" ({h.hop}-hop{': ' + h.via if h.via else ''})" if h.hop else ""
        line = f"- {h.triple.full_text}{tag}"
        if used + len(line) + 1 > max_chars:
            break
        lines.append(line)
        used += len(line) + 1
    return "\n".join(lines)
