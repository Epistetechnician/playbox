"""Text embedders for the layout. Pick with PLAYBOX_EMBEDDER=hash|lsa|st (default lsa).

hash  hashed bag of words. No fitting, no semantics beyond shared words.
lsa   TF-IDF then truncated SVD, fitted on this room's own events. Words that appear in the same
      events end up close, so two events on one topic that share no word can still land together.
st    sentence-transformers, if installed (it downloads a model on first use, so it is opt-in).
"""

import os
import re

import numpy as np

from .events import embed as hash_embed

_TOK = re.compile(r"[a-z0-9_]{2,}")


class HashEmbedder:
    dim = 256

    def fit(self, texts):
        return self

    def __call__(self, texts):
        return np.stack([hash_embed(t) for t in texts])


class LSAEmbedder:
    def __init__(self, dim=48):
        self.k = dim
        self.vocab, self.idf, self.proj = {}, None, None

    @property
    def dim(self):
        return self.proj.shape[1] if self.proj is not None else self.k

    def _tf(self, texts):
        m = np.zeros((len(texts), len(self.vocab)), dtype=np.float32)
        for i, t in enumerate(texts):
            for w in _TOK.findall(t.lower()):
                if w in self.vocab:
                    m[i, self.vocab[w]] += 1
        return np.log1p(m)

    def fit(self, texts):
        words = sorted({w for t in texts for w in _TOK.findall(t.lower())})
        self.vocab = {w: i for i, w in enumerate(words)}
        tf = self._tf(texts)
        df = (tf > 0).sum(0)
        self.idf = np.log((1 + len(texts)) / (1 + df)).astype(np.float32) + 1
        x = tf * self.idf
        k = max(2, min(self.k, min(x.shape) - 1))
        _, _, vt = np.linalg.svd(x, full_matrices=False)
        self.proj = vt[:k].T.astype(np.float32)
        return self

    def __call__(self, texts):
        z = (self._tf(texts) * self.idf) @ self.proj
        n = np.linalg.norm(z, axis=1, keepdims=True)
        return z / np.maximum(n, 1e-8)


class SentenceTransformerEmbedder:
    def __init__(self, model="all-MiniLM-L6-v2"):
        from sentence_transformers import SentenceTransformer  # opt-in dependency

        self.m = SentenceTransformer(model)
        self.dim = self.m.get_sentence_embedding_dimension()

    def fit(self, texts):
        return self

    def __call__(self, texts):
        return np.asarray(self.m.encode(list(texts), normalize_embeddings=True), dtype=np.float32)


def get_embedder(name=None):
    name = name or os.environ.get("PLAYBOX_EMBEDDER", "lsa")
    return {"hash": HashEmbedder, "lsa": LSAEmbedder, "st": SentenceTransformerEmbedder}[name]()
