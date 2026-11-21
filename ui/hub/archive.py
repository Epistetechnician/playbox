"""A durable copy of a room's feed, kept by the hub.

The host of a room keeps the chat (`chat.jsonl`), but a hub that joined someone's room (or adopted one) only ever *reads* it, live. When the host
goes away, or its bounded chat window evicts old lines, the hub had nothing to show. This keeps what the hub has seen, on disk, per room:

    <room home>/archive/meta.json        counters, the cursor, the last member list, the segment index
    <room home>/archive/feed-0000.jsonl  one JSON object per line: a feed item (seq, kind, from, name, text, time_ms) plus "epoch"

Append-only, so a crash can at worst tear the last line, which readers skip. Segments roll at SEGMENT_BYTES and the oldest are deleted past MAX_BYTES,
so a room cannot fill the disk. Directories are 0700 and files 0600; the text is other people's words and is stored as data, never interpreted.
`epoch` counts how many times the host's sequence numbers started over (a host that lost its state), so two different histories never merge.
"""
import json
import os
import threading
import time
from pathlib import Path

SEGMENT_BYTES = 8 * 1024 * 1024
MAX_BYTES = int(os.environ.get("PLAYBOX_HUB_ARCHIVE_MAX_MB", "256")) * 1024 * 1024
MAX_ITEM_BYTES = 16 * 1024
KEEP_FIELDS = ("seq", "kind", "from", "name", "text", "time_ms", "meta")
META_KEYS = frozenset({
    "event", "ok", "ms", "version", "transport", "runner", "isolation", "repo", "replica",
    "scratch_quota_mib", "max_push_mib", "program", "args", "cmd_hash", "image", "base",
    "base_commit", "task_id", "replay_task_id", "patch_hash", "stdout_hash", "patch_bytes",
    "reproducible", "patch_match", "policy_match", "exit_match",
    "state", "exit_code", "timed_out", "cpus", "mem_mib", "timeout_secs", "network",
    "warm", "keep", "sync", "policy_digest", "commit", "branch", "changed_files",
    "egress", "secrets", "ref", "reference", "bytes", "reason", "kind", "model",
    "why", "limit_bytes", "authenticated", "conv", "roster",
    "id", "to", "open_cards", "verified_artifacts", "failed_checks",
    "task_budget", "max_concurrent", "max_tasks_per_minute", "memory_review",
    "doc", "key", "taint",
})


def _clean_str(v, limit):
    return "".join(ch for ch in str(v) if ord(ch) >= 32)[:limit].strip()


def _clean_meta(m):
    """Allowlisted, bounded telemetry dict from an event item. Untrusted host data is clipped and never nested."""
    if not isinstance(m, dict):
        return None
    out = {}
    for k, v in m.items():
        if k not in META_KEYS:
            continue
        if isinstance(v, bool):
            out[k] = v
        elif isinstance(v, (int, float)):
            if v == v and abs(v) < 1e15:
                out[k] = v
        elif isinstance(v, str):
            s = _clean_str(v, 120)
            if s:
                out[k] = s
        elif isinstance(v, list):
            lst = [_clean_str(x, 60) for x in v[:8] if isinstance(x, str)]
            lst = [x for x in lst if x]
            if lst:
                out[k] = lst
    return out or None


def _clean(item):
    """The fields we keep, with an upper bound on size. Anything else the host sent is dropped."""
    out = {k: item.get(k) for k in KEEP_FIELDS if k in item and k != "meta"}
    if isinstance(out.get("text"), str) and len(out["text"]) > 8000:
        out["text"] = out["text"][:8000]
    if "meta" in item:
        cm = _clean_meta(item.get("meta"))
        if cm:
            out["meta"] = cm
    return out


class Archive:
    def __init__(self, room_home, segment_bytes=SEGMENT_BYTES, max_bytes=MAX_BYTES):
        self.dir = Path(room_home) / "archive"
        self.segment_bytes, self.max_bytes = segment_bytes, max_bytes
        self.mu = threading.RLock()
        self.meta = None

    # ---- state ----
    def _load(self):
        if self.meta is not None:
            return self.meta
        meta = None
        try:
            meta = json.loads((self.dir / "meta.json").read_text())
            if not isinstance(meta, dict) or not isinstance(meta.get("segments"), list):
                meta = None
        except (OSError, ValueError):
            meta = None
        if meta is None:
            meta = self._rebuild()
        self.meta = meta
        return meta

    def _rebuild(self):
        """meta.json missing or damaged: recover the index from the segment files themselves."""
        meta = {"epoch": 0, "last_seq": 0, "count": 0, "last_sync_ms": None, "members": [], "me": None, "segments": []}
        for f in sorted(self.dir.glob("feed-*.jsonl")) if self.dir.is_dir() else []:
            first = last = ep = None
            n = 0
            for it in self._lines(f):
                n += 1
                first = it["seq"] if first is None else first
                ep = int(it.get("epoch", 0)) if ep is None else ep
                last = it["seq"]
            if n:
                meta["epoch"] = max(meta["epoch"], ep)
                meta["segments"].append({"name": f.name, "first_seq": first, "last_seq": last, "count": n, "bytes": f.stat().st_size, "epoch": ep})
                meta["last_seq"], meta["count"] = last, meta["count"] + n
        return meta

    def _save(self):
        self.dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        tmp = self.dir / "meta.json.tmp"
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            json.dump(self.meta, f, separators=(",", ":"))
        os.replace(tmp, self.dir / "meta.json")

    @staticmethod
    def _lines(path):
        """Items in a segment, in order. A torn or garbled line (a crash mid-write) is skipped."""
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as f:
                for ln in f:
                    try:
                        it = json.loads(ln)
                    except ValueError:
                        continue
                    if isinstance(it, dict) and isinstance(it.get("seq"), int):
                        yield it
        except OSError:
            return

    # ---- writing ----
    def append(self, items, members=None, me=None, now_ms=None):
        """Store feed items not stored yet (by sequence number). Returns how many were new."""
        with self.mu:
            meta = self._load()
            uniq = {i["seq"]: _clean(i) for i in items if isinstance(i, dict) and isinstance(i.get("seq"), int) and i["seq"] > meta["last_seq"]}
            fresh = [uniq[k] for k in sorted(uniq)]
            changed = False
            if members is not None and members != meta.get("members"):
                meta["members"] = [{"id": str(m.get("id", ""))[:80], "name": (m.get("name") or None), "last_seen_ms": m.get("last_seen_ms")} for m in members if isinstance(m, dict)][:200]
                changed = True
            if me and me != meta.get("me"):
                meta["me"], changed = me, True
            if not fresh:
                if changed:
                    self._save()
                return 0
            self.dir.mkdir(parents=True, exist_ok=True, mode=0o700)
            seg = meta["segments"][-1] if meta["segments"] else None
            if seg is None or seg["bytes"] >= self.segment_bytes or meta.pop("new_segment", False):
                n = int(seg["name"][5:9]) + 1 if seg else 0
                seg = {"name": f"feed-{n:04d}.jsonl", "first_seq": fresh[0]["seq"], "last_seq": fresh[0]["seq"], "count": 0, "bytes": 0, "epoch": meta["epoch"]}
                meta["segments"].append(seg)
            path = self.dir / seg["name"]
            torn = False
            try:
                with open(path, "rb") as probe:                 # a crash can leave the last line without its newline: end it, so the next line is not glued on
                    probe.seek(0, os.SEEK_END)
                    if probe.tell():
                        probe.seek(-1, os.SEEK_END)
                        torn = probe.read(1) != b"\n"
            except OSError:
                pass
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
            try:
                with os.fdopen(fd, "ab") as f:
                    if torn:
                        f.write(b"\n")
                        seg["bytes"] += 1
                    for it in fresh:
                        it["epoch"] = meta["epoch"]
                        line = (json.dumps(it, separators=(",", ":"), ensure_ascii=False) + "\n").encode()
                        if len(line) > MAX_ITEM_BYTES:
                            it["text"] = (it.get("text") or "")[:2000]
                            line = (json.dumps(it, separators=(",", ":"), ensure_ascii=False) + "\n").encode()
                        f.write(line)
                        seg["bytes"] += len(line)
                        seg["count"] += 1
                        seg["last_seq"] = it["seq"]
                        meta["count"] += 1
                    f.flush()
                    os.fsync(f.fileno())
            finally:
                meta["last_seq"] = max(meta["last_seq"], seg["last_seq"])
            meta["last_sync_ms"] = now_ms or int(time.time() * 1000)
            self._trim()
            self._save()
            return len(fresh)

    def _trim(self):
        meta = self.meta
        while len(meta["segments"]) > 1 and sum(s["bytes"] for s in meta["segments"]) > self.max_bytes:
            old = meta["segments"].pop(0)
            meta["count"] -= old["count"]
            try:
                (self.dir / old["name"]).unlink()
            except OSError:
                pass

    def host_restarted(self):
        """The host's sequence numbers started over (it lost its state). Keep what we have as an earlier epoch and begin again from zero."""
        with self.mu:
            meta = self._load()
            if meta["last_seq"] == 0:
                return
            meta["epoch"] += 1
            meta["last_seq"] = 0
            meta["new_segment"] = True          # the new epoch starts in a segment of its own
            self._save()

    # ---- reading ----
    def info(self):
        with self.mu:
            m = self._load()
            return {"items": m["count"], "last_seq": m["last_seq"], "last_sync_ms": m["last_sync_ms"], "epoch": m["epoch"],
                    "bytes": sum(s["bytes"] for s in m["segments"]), "segments": len(m["segments"])}

    def members(self):
        with self.mu:
            m = self._load()
            return list(m.get("members") or []), m.get("me")

    def read(self, since=0, limit=500, all_epochs=False):
        """Items after `since` in the current epoch, oldest first, at most `limit`. Returns (items, next)."""
        with self.mu:
            meta = self._load()
            out = []
            for seg in meta["segments"]:
                if not all_epochs and seg.get("epoch", 0) != meta["epoch"]:
                    continue
                if seg["last_seq"] <= since and not all_epochs:
                    continue
                for it in self._lines(self.dir / seg["name"]):
                    if not all_epochs and (it.get("epoch", 0) != meta["epoch"] or it["seq"] <= since):
                        continue
                    out.append(it)
                    if len(out) >= limit:
                        return out, out[-1]["seq"]
            return out, (out[-1]["seq"] if out else since)

    def tail(self, n=1000):
        """The newest `n` items of the current epoch, oldest first. Reads only as many segments as it needs."""
        with self.mu:
            meta = self._load()
            chunks, got = [], 0
            for seg in reversed(meta["segments"]):
                if seg.get("epoch", 0) != meta["epoch"]:
                    break
                items = [it for it in self._lines(self.dir / seg["name"]) if it.get("epoch", 0) == meta["epoch"]]
                chunks.append(items)
                got += len(items)
                if got >= n:
                    break
            flat = [it for chunk in reversed(chunks) for it in chunk]
            return flat[-n:]

    def export(self):
        """Every stored line, all epochs, oldest first, as bytes (the segments are already JSON lines)."""
        with self.mu:
            meta = self._load()
            return b"".join((json.dumps(it, separators=(",", ":"), ensure_ascii=False) + "\n").encode() for seg in meta["segments"] for it in self._lines(self.dir / seg["name"]))

    def delete(self):
        with self.mu:
            import shutil
            shutil.rmtree(self.dir, ignore_errors=True)
            self.meta = None
