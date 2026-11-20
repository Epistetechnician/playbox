"""Zero-GPU dense embedding & rerank adapters for `playbox_atlas` backed by `playbox-aiproxy`.

Security invariant:
- `HiKVPIndex` enforces `TRUSTED_TIERS` quarantine filtering *before* invoking `embed_fn` or `rerank_fn`,
  so `quarantined` / `untrusted` room-memory triples are never sent to external providers or promoted.
- Embeddings are cached locally in SQLite keyed by `sha256(model + "\\n" + text)` for near-zero incremental cost.
- Network errors degrade gracefully so `HiKVPIndex` falls back to pure-stdlib TF-IDF when offline.
"""
from __future__ import annotations

import hashlib
import json
import sqlite3
import urllib.error
import urllib.request
from pathlib import Path
from typing import Sequence


class _EmbedCache:
    """Content-addressed SQLite vector cache keyed by `(model, text_sha256)`."""

    def __init__(self, path: str | Path):
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._conn = sqlite3.connect(str(self.path))
        self._conn.execute(
            "CREATE TABLE IF NOT EXISTS embeddings ("
            "key TEXT PRIMARY KEY, "
            "vec TEXT NOT NULL)"
        )
        self._conn.commit()

    @staticmethod
    def _key(model: str, text: str) -> str:
        return hashlib.sha256(f"{model}\n{text}".encode("utf-8", "ignore")).hexdigest()

    def get_many(self, model: str, texts: Sequence[str]) -> dict[int, list[float]]:
        out: dict[int, list[float]] = {}
        keys = [self._key(model, t) for t in texts]
        for start in range(0, len(keys), 200):
            chunk = keys[start : start + 200]
            qmarks = ",".join("?" for _ in chunk)
            rows = self._conn.execute(
                f"SELECT key, vec FROM embeddings WHERE key IN ({qmarks})", chunk
            ).fetchall()
            found = {k: json.loads(v) for k, v in rows}
            for off, k in enumerate(chunk):
                if k in found:
                    out[start + off] = found[k]
        return out

    def put_many(self, model: str, pairs: Sequence[tuple[str, Sequence[float]]]) -> None:
        if not pairs:
            return
        rows = [(self._key(model, text), json.dumps(list(vec))) for text, vec in pairs]
        self._conn.executemany(
            "INSERT OR REPLACE INTO embeddings (key, vec) VALUES (?, ?)", rows
        )
        self._conn.commit()

    def close(self) -> None:
        self._conn.close()


class AiproxyEmbedder:
    """Callable `embed_fn` + batched `embed_batch` client for `playbox-aiproxy` `/v1/embeddings`."""

    def __init__(
        self,
        base_url: str,
        api_key: str,
        *,
        model: str = "text-embedding-3-small",
        cache_path: str | Path | None = None,
        batch_size: int = 64,
        timeout: float = 15.0,
        extra_headers: dict[str, str] | None = None,
    ):
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key.strip()
        self.model = model
        self.batch_size = max(1, int(batch_size))
        self.timeout = float(timeout)
        self.extra_headers = dict(extra_headers or {})
        self._cache = _EmbedCache(cache_path) if cache_path else None
        self.api_calls = 0
        self.cache_hits = 0

    def _post_batch(self, texts: Sequence[str]) -> list[list[float]]:
        payload = json.dumps({"model": self.model, "input": list(texts)}).encode("utf-8")
        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
            "Accept": "application/json",
            "User-Agent": "playbox-atlas/0.4",
            **self.extra_headers,
        }
        req = urllib.request.Request(
            f"{self.base_url}/v1/embeddings",
            method="POST",
            data=payload,
            headers=headers,
        )
        with urllib.request.urlopen(req, timeout=self.timeout) as resp:
            raw = json.loads(resp.read(32 * 1024 * 1024) or b"{}")
        self.api_calls += 1
        data = raw.get("data") or []
        data_sorted = sorted(data, key=lambda item: int(item.get("index", 0)))
        if len(data_sorted) != len(texts):
            raise ValueError(f"embedding count mismatch: got {len(data_sorted)} for {len(texts)} inputs")
        return [[float(x) for x in item["embedding"]] for item in data_sorted]

    def embed_batch(self, texts: Sequence[str]) -> list[list[float]]:
        """Embed a sequence of strings using local SHA-256 cache + batched `/v1/embeddings` calls."""
        if not texts:
            return []
        cached = self._cache.get_many(self.model, texts) if self._cache else {}
        self.cache_hits += len(cached)
        missing_idxs = [i for i in range(len(texts)) if i not in cached]
        result: list[list[float] | None] = [cached.get(i) for i in range(len(texts))]

        to_store: list[tuple[str, list[float]]] = []
        for start in range(0, len(missing_idxs), self.batch_size):
            batch_idxs = missing_idxs[start : start + self.batch_size]
            batch_texts = [texts[i] for i in batch_idxs]
            vecs = self._post_batch(batch_texts)
            for idx, text, vec in zip(batch_idxs, batch_texts, vecs):
                result[idx] = vec
                to_store.append((text, vec))
        if self._cache and to_store:
            self._cache.put_many(self.model, to_store)
        return [v if v is not None else [] for v in result]

    def __call__(self, text: str) -> list[float]:
        vecs = self.embed_batch([text])
        return vecs[0] if vecs else []

    def close(self) -> None:
        if self._cache:
            self._cache.close()


class AiproxyReranker:
    """Optional second-stage reranker backed by `playbox-aiproxy` `/v1/rerank`."""

    def __init__(
        self,
        base_url: str,
        api_key: str,
        *,
        model: str = "rerank-2",
        timeout: float = 15.0,
        extra_headers: dict[str, str] | None = None,
    ):
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key.strip()
        self.model = model
        self.timeout = float(timeout)
        self.extra_headers = dict(extra_headers or {})
        self.api_calls = 0

    def __call__(self, query: str, documents: Sequence[str], *, top_n: int | None = None) -> list[tuple[int, float]]:
        """Return `[(doc_index, relevance_score), ...]` sorted descending by score."""
        if not documents:
            return []
        body: dict[str, object] = {
            "model": self.model,
            "query": query,
            "documents": list(documents),
        }
        if top_n is not None and top_n > 0:
            body["top_n"] = min(int(top_n), len(documents))
        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
            "Accept": "application/json",
            "User-Agent": "playbox-atlas/0.4",
            **self.extra_headers,
        }
        req = urllib.request.Request(
            f"{self.base_url}/v1/rerank",
            method="POST",
            data=json.dumps(body).encode("utf-8"),
            headers=headers,
        )
        with urllib.request.urlopen(req, timeout=self.timeout) as resp:
            raw = json.loads(resp.read(16 * 1024 * 1024) or b"{}")
        self.api_calls += 1
        results = raw.get("results") or raw.get("data") or []
        scored = [
            (int(item["index"]), float(item.get("relevance_score", item.get("score", 0.0))))
            for item in results
            if "index" in item
        ]
        scored.sort(key=lambda x: x[1], reverse=True)
        return scored
