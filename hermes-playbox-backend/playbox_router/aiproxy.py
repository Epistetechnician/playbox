"""Host-side client and canonical ledger verifier for `playbox-aiproxy`.

Pure stdlib (`urllib.request`, `hashlib`, `json`) with loopback/UDS safety:
- Mints short-lived per-worker tokens with hard USD/TPM ceilings, model allow-lists,
  failover policies (`same_model_only` vs `alias_cascade`), and cache modes (`off|record|replay`).
- Revokes tokens immediately on worker exit or member revocation.
- Fetches and cryptographically verifies `playbox.llm_ledger.v1` canonical SHA-256 digests
  before binding them into `playbox.attested_gate_receipt.v1`.
"""
from __future__ import annotations

import hashlib
import json
import os
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

LEDGER_SCHEMA = "playbox.llm_ledger.v1"
VALID_FAILOVER_POLICIES = frozenset({"same_model_only", "alias_cascade"})
VALID_CACHE_MODES = frozenset({"off", "record", "replay"})


class AiproxyError(RuntimeError):
    """Raised when `playbox-aiproxy` rejects a control-plane request or is unreachable."""

    def __init__(self, status: int, message: str):
        super().__init__(message)
        self.status = status
        self.message = message


def _jcs_encode(obj: Any) -> str:
    """RFC 8785 JCS / Go `encoding/json`-compatible canonical serializer (including `[1e-6, 1e-4)` fixed-point floats)."""
    if obj is None:
        return "null"
    if isinstance(obj, bool):
        return "true" if obj else "false"
    if isinstance(obj, int):
        return str(obj)
    if isinstance(obj, float):
        if obj.is_integer():
            return str(int(obj))
        if 1e-6 <= abs(obj) < 1.0:
            return f"{obj:.12f}".rstrip("0").rstrip(".")
        return json.dumps(obj)
    if isinstance(obj, str):
        return json.dumps(obj, ensure_ascii=True)
    if isinstance(obj, list):
        return "[" + ",".join(_jcs_encode(x) for x in obj) + "]"
    if isinstance(obj, dict):
        items = [
            f"{json.dumps(str(k), ensure_ascii=True)}:{_jcs_encode(obj[k])}"
            for k in sorted(obj.keys())
        ]
        return "{" + ",".join(items) + "}"
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def canonical_ledger_bytes(ledger: dict[str, Any]) -> bytes:
    """Deterministic UTF-8 canonical JSON encoding matching Go `playbox.CanonicalLedgerBytes`."""
    body = {k: v for k, v in ledger.items() if k != "ledger_sha256"}
    return _jcs_encode(body).encode("utf-8")


def compute_ledger_sha256(ledger: dict[str, Any]) -> str:
    """Compute SHA-256 hex digest over a `playbox.llm_ledger.v1` document (excluding `ledger_sha256`)."""
    return hashlib.sha256(canonical_ledger_bytes(ledger)).hexdigest()


def verify_ledger(ledger: dict[str, Any]) -> tuple[bool, str]:
    """Verify schema and canonical SHA-256 digest of a `playbox.llm_ledger.v1` document."""
    if not isinstance(ledger, dict):
        return False, "ledger must be a JSON object"
    if ledger.get("schema") != LEDGER_SCHEMA:
        return False, f"unexpected ledger schema: {ledger.get('schema')!r}"
    claimed = str(ledger.get("ledger_sha256") or "")
    if len(claimed) != 64:
        return False, "missing or malformed ledger_sha256"
    computed = compute_ledger_sha256(ledger)
    if computed != claimed.lower():
        return False, f"ledger_sha256 mismatch (computed {computed[:12]} != claimed {claimed[:12]})"
    return True, "ok"


def summarize_ledger_for_receipt(
    ledger: dict[str, Any],
    *,
    kv_summary: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Produce the compact `llm_ledger` block embedded in `playbox.attested_gate_receipt.v1`."""
    ok, reason = verify_ledger(ledger)
    if not ok:
        raise ValueError(f"cannot bind invalid ledger into receipt: {reason}")
    totals = dict(ledger.get("totals") or {})
    cached_read = int(totals.get("cached_read_tokens", 0) or 0)
    cached_write = int(totals.get("cached_write_tokens", 0) or 0)
    cached_total = int(totals.get("cached_tokens", cached_read + cached_write) or 0)
    out = {
        "source": "host_aiproxy",
        "schema": LEDGER_SCHEMA,
        "ledger_sha256": str(ledger["ledger_sha256"]).lower(),
        "gateway_digest": str(ledger.get("gateway_digest") or ""),
        "room_id": str(ledger.get("room_id") or ""),
        "run_id": str(ledger.get("run_id") or ""),
        "cache_mode": str(ledger.get("cache_mode") or "off"),
        "failover_policy": str(ledger.get("failover_policy") or "same_model_only"),
        "body_retention": bool(ledger.get("body_retention", False)),
        "totals": {
            "requests": int(totals.get("requests", 0) or 0),
            "retries": int(totals.get("retries", 0) or 0),
            "failovers": int(totals.get("failovers", 0) or 0),
            "budget_cutoffs": int(totals.get("budget_cutoffs", 0) or 0),
            "input_tokens": int(totals.get("input_tokens", 0) or 0),
            "cached_tokens": cached_total,
            "cached_read_tokens": cached_read,
            "cached_write_tokens": cached_write,
            "output_tokens": int(totals.get("output_tokens", 0) or 0),
            "reasoning_tokens": int(totals.get("reasoning_tokens", 0) or 0),
            "cost_usd": round(float(totals.get("cost_usd", 0.0) or 0.0), 6),
            "median_ttft_ms": int(totals.get("median_ttft_ms", 0) or 0),
            "p95_latency_ms": int(totals.get("p95_latency_ms", 0) or 0),
            "models_used": sorted({str(m) for m in (totals.get("models_used") or []) if m}),
        },
    }
    if kv_summary is not None:
        out["kv_gateway"] = dict(kv_summary)
    return out


@dataclass(frozen=True)
class MintedWorkerToken:
    token_id: int
    key: str
    name: str
    group: str
    hard_usd_cap: float
    tpm_cap: int = 0
    failover_policy: str = "same_model_only"
    cache_mode: str = "off"


def _validate_base_url(base_url: str, *, allow_non_loopback: bool = False) -> str:
    parsed = urllib.parse.urlparse(base_url)
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        raise ValueError(f"invalid playbox-aiproxy base_url: {base_url!r}")
    host = parsed.hostname.lower()
    if not allow_non_loopback and host not in ("127.0.0.1", "localhost", "::1"):
        raise ValueError(
            f"refusing non-loopback playbox-aiproxy URL {base_url!r} without allow_non_loopback=True"
        )
    return base_url.rstrip("/")


def load_admin_key(key_or_path: str | Path) -> str:
    """Load an `ADMIN_KEY` either from a `0600` file path or an explicit env string."""
    p = Path(key_or_path)
    if p.is_file():
        return p.read_text(encoding="utf-8").strip()
    return str(key_or_path).strip()


class AiproxyClient:
    """Host-side management client for `playbox-aiproxy`."""

    def __init__(
        self,
        base_url: str,
        admin_key: str,
        *,
        timeout: float = 15.0,
        allow_non_loopback: bool = False,
    ):
        self.base_url = _validate_base_url(base_url, allow_non_loopback=allow_non_loopback)
        self.admin_key = admin_key.strip()
        if not self.admin_key:
            raise ValueError("playbox-aiproxy admin_key must not be empty")
        self.timeout = float(timeout)

    def _req(
        self,
        method: str,
        path: str,
        *,
        body: dict[str, Any] | None = None,
        query: dict[str, Any] | None = None,
    ) -> Any:
        qs = ""
        if query:
            clean_q = {k: str(v) for k, v in query.items() if v is not None and v != ""}
            if clean_q:
                qs = "?" + urllib.parse.urlencode(clean_q)
        url = f"{self.base_url}{path}{qs}"
        data = None if body is None else json.dumps(body).encode("utf-8")
        req = urllib.request.Request(
            url,
            method=method,
            data=data,
            headers={
                "Authorization": self.admin_key,
                "Content-Type": "application/json",
                "Accept": "application/json",
                "User-Agent": "playbox-router/0.4",
            },
        )
        for attempt in range(5):
            try:
                with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                    raw = resp.read(16 * 1024 * 1024)
                break
            except urllib.error.HTTPError as exc:
                detail = ""
                try:
                    detail = exc.read(4096).decode("utf-8", "replace")
                except OSError:
                    pass
                if exc.code >= 500 and "SQLITE_BUSY" in detail and attempt < 4:
                    time.sleep(0.2 * (attempt + 1))
                    continue
                raise AiproxyError(exc.code, f"playbox-aiproxy {method} {path} -> HTTP {exc.code}: {detail}") from exc
            except (OSError, urllib.error.URLError) as exc:
                raise AiproxyError(502, f"playbox-aiproxy unreachable at {self.base_url}: {exc}") from exc
        try:
            parsed = json.loads(raw or b"{}")
        except ValueError as exc:
            raise AiproxyError(502, f"playbox-aiproxy returned invalid JSON on {path}") from exc
        if isinstance(parsed, dict) and parsed.get("success") is False:
            raise AiproxyError(400, str(parsed.get("message") or f"aiproxy error on {path}"))
        return parsed.get("data") if isinstance(parsed, dict) and "data" in parsed else parsed

    def posture(self) -> dict[str, Any]:
        """Return hardened gateway build & runtime posture."""
        res = self._req("GET", "/api/playbox/posture")
        return dict(res or {})

    def ensure_group(self, group: str, *, rpm_ratio: float = 1.0, tpm_ratio: float = 1.0) -> dict[str, Any]:
        """Create or return an isolated `aiproxy` Group (e.g. `room:<id>`, `bench:solver`, `bench:judge`)."""
        try:
            return dict(self._req("GET", f"/api/group/{urllib.parse.quote(group, safe='')}"))
        except AiproxyError as exc:
            if exc.status not in (400, 404) and "not found" not in exc.message.lower():
                raise
        try:
            return dict(
                self._req(
                    "POST",
                    f"/api/group/{urllib.parse.quote(group, safe='')}",
                    body={"rpm_ratio": rpm_ratio, "tpm_ratio": tpm_ratio},
                )
                or {}
            )
        except AiproxyError as exc:
            if "duplicated" in exc.message.lower() or "exists" in exc.message.lower():
                return dict(self._req("GET", f"/api/group/{urllib.parse.quote(group, safe='')}") or {})
            raise

    def mint_worker_token(
        self,
        group: str,
        name: str,
        *,
        hard_usd_cap: float,
        tpm_cap: int = 0,
        models: list[str] | None = None,
        subnets: list[str] | None = None,
        failover_policy: str = "same_model_only",
        cache_mode: str = "off",
        run_id: str = "",
        room_id: str = "",
        member_id: str = "",
        task_id: str = "",
    ) -> MintedWorkerToken:
        """Mint a short-lived, hard-capped worker token locked to loopback by default."""
        if failover_policy not in VALID_FAILOVER_POLICIES:
            raise ValueError(f"invalid failover_policy: {failover_policy!r}")
        if cache_mode not in VALID_CACHE_MODES:
            raise ValueError(f"invalid cache_mode: {cache_mode!r}")
        if hard_usd_cap <= 0:
            raise ValueError("hard_usd_cap must be positive")
        unique_name = f"{name[:25]}-{os.urandom(3).hex()}"
        payload: dict[str, Any] = {
            "name": unique_name,
            "quota": float(hard_usd_cap),
            "subnets": list(subnets if subnets is not None else ["127.0.0.1/32", "::1/128"]),
        }
        if models:
            payload["models"] = list(models)
        if tpm_cap > 0:
            payload["tpm_cap"] = int(tpm_cap)
        res = self._req(
            "POST",
            f"/api/token/{urllib.parse.quote(group, safe='')}",
            body=payload,
        )
        if not isinstance(res, dict) or not res.get("key"):
            raise AiproxyError(502, f"unexpected mint_worker_token response: {res!r}")
        tok_id = int(res.get("id", 0))
        if tok_id > 0:
            try:
                self._req(
                    "POST",
                    "/api/playbox/token_meta",
                    body={
                        "token_id": tok_id,
                        "run_id": run_id,
                        "room_id": room_id,
                        "member_id": member_id or name[:32],
                        "task_id": task_id,
                        "cache_mode": cache_mode,
                        "failover_policy": failover_policy,
                    },
                )
            except AiproxyError:
                pass
        return MintedWorkerToken(
            token_id=tok_id,
            key=str(res["key"]),
            name=str(res.get("name") or name[:32]),
            group=group,
            hard_usd_cap=float(hard_usd_cap),
            tpm_cap=int(tpm_cap),
            failover_policy=failover_policy,
            cache_mode=cache_mode,
        )

    def revoke_token(self, group: str, token_id: int) -> bool:
        """Immediately disable/delete a worker or member token on exit or revocation."""
        try:
            self._req(
                "POST",
                f"/api/token/{urllib.parse.quote(group, safe='')}/{int(token_id)}/status",
                body={"status": 2},
            )
            return True
        except AiproxyError:
            try:
                self._req(
                    "DELETE",
                    f"/api/token/{urllib.parse.quote(group, safe='')}/{int(token_id)}",
                )
                return True
            except AiproxyError:
                return False

    def fetch_ledger(
        self,
        *,
        run_id: str = "",
        group: str = "",
        token_id: int | None = None,
        task_id: str = "",
        since_ms: int | None = None,
        until_ms: int | None = None,
    ) -> dict[str, Any]:
        """Fetch and verify the host-side `playbox.llm_ledger.v1` canonical usage ledger."""
        res = self._req(
            "GET",
            "/api/playbox/ledger",
            query={
                "run_id": run_id,
                "group": group,
                "token_id": token_id if token_id is not None else "",
                "task_id": task_id,
                "since_ms": since_ms if since_ms is not None else "",
                "until_ms": until_ms if until_ms is not None else "",
            },
        )
        if not isinstance(res, dict):
            raise AiproxyError(502, "ledger response was not a JSON object")
        ok, reason = verify_ledger(res)
        if not ok:
            raise AiproxyError(502, f"host ledger failed integrity check: {reason}")
        return res

    def build_egress_config(
        self,
        *,
        secret_name: str = "aiproxy_worker_token",
        upstream_name: str = "llm",
        room_id: str = "",
        member_id: str = "",
        task_id: str = "",
        run_id: str = "",
        cache_mode: str = "off",
        failover_policy: str = "same_model_only",
        log_file: str | None = None,
    ) -> dict[str, Any]:
        """Generate a fail-closed `BOX-7` `EgressConfig` JSON document pointing at this gateway."""
        extra_headers: dict[str, str] = {
            "x-playbox-cache-mode": cache_mode,
            "x-playbox-failover-policy": failover_policy,
        }
        if room_id:
            extra_headers["x-playbox-room-id"] = room_id
        if member_id:
            extra_headers["x-playbox-member-id"] = member_id
        if task_id:
            extra_headers["x-playbox-task-id"] = task_id
        if run_id:
            extra_headers["x-playbox-run-id"] = run_id

        parsed = urllib.parse.urlparse(self.base_url)
        is_loopback_http = parsed.scheme == "http" and (parsed.hostname or "").lower() in (
            "127.0.0.1",
            "localhost",
            "::1",
        )
        cfg: dict[str, Any] = {
            "upstreams": [
                {
                    "name": upstream_name,
                    "base": self.base_url,
                    "auth_header": "authorization",
                    "auth_prefix": "Bearer ",
                    "secret": secret_name,
                    "allow_paths": [
                        "/v1/messages",
                        "/v1/chat/completions",
                        "/v1/responses",
                        "/v1/embeddings",
                        "/v1/rerank",
                        "/v1beta/models/*",
                    ],
                    "pass_headers": [
                        "content-type",
                        "accept",
                        "user-agent",
                        "anthropic-version",
                        "anthropic-beta",
                        "openai-beta",
                    ],
                    "extra_headers": extra_headers,
                    "env_var": "PLAYBOX_AIPROXY_URL",
                    "insecure_http": is_loopback_http,
                    "allow_private": True,
                }
            ]
        }
        if log_file:
            cfg["log_file"] = log_file
        return cfg

    @classmethod
    def from_env(cls) -> AiproxyClient | None:
        """Construct a client from `PLAYBOX_AIPROXY_URL` and `PLAYBOX_AIPROXY_ADMIN_KEY(_FILE)` if configured."""
        url = os.environ.get("PLAYBOX_AIPROXY_URL", "").strip()
        key_file = os.environ.get("PLAYBOX_AIPROXY_ADMIN_KEY_FILE", "").strip()
        key = os.environ.get("PLAYBOX_AIPROXY_ADMIN_KEY", "").strip()
        if not url or (not key and not key_file):
            return None
        resolved_key = load_admin_key(key_file or key)
        if not resolved_key:
            return None
        return cls(url, resolved_key)

    def worker_lease(
        self,
        *,
        group: str,
        worker_label: str,
        hard_usd_cap: float,
        run_id: str = "",
        room_id: str = "",
        task_id: str = "",
        models: list[str] | None = None,
        tpm_cap: int = 0,
        failover_policy: str = "same_model_only",
        cache_mode: str = "off",
    ) -> WorkerTokenLease:
        """Context manager that mints a short-lived hard-capped worker token, exposes unified
        Anthropic/OpenAI/Gemini env vars, revokes the token unconditionally on exit, and fetches
        the host-attested `playbox.llm_ledger.v1` for that worker."""
        return WorkerTokenLease(
            client=self,
            group=group,
            worker_label=worker_label,
            hard_usd_cap=hard_usd_cap,
            run_id=run_id,
            room_id=room_id,
            task_id=task_id,
            models=models,
            tpm_cap=tpm_cap,
            failover_policy=failover_policy,
            cache_mode=cache_mode,
        )


class WorkerTokenLease:
    """Short-lived, hard-capped `playbox-aiproxy` worker token lease with guaranteed revocation."""

    def __init__(
        self,
        *,
        client: AiproxyClient,
        group: str,
        worker_label: str,
        hard_usd_cap: float,
        run_id: str = "",
        room_id: str = "",
        task_id: str = "",
        models: list[str] | None = None,
        tpm_cap: int = 0,
        failover_policy: str = "same_model_only",
        cache_mode: str = "off",
    ):
        self.client = client
        self.group = group
        self.worker_label = worker_label
        self.hard_usd_cap = max(0.001, float(hard_usd_cap))
        self.run_id = run_id
        self.room_id = room_id
        self.task_id = task_id
        self.models = list(models) if models else None
        self.tpm_cap = int(tpm_cap)
        self.failover_policy = failover_policy
        self.cache_mode = cache_mode
        self.token: MintedWorkerToken | None = None
        self.ledger: dict[str, Any] | None = None
        self.revoked: bool = False

    def __enter__(self) -> WorkerTokenLease:
        self.client.ensure_group(self.group)
        tok_name = f"{self.run_id[:16]}-{self.worker_label}" if self.run_id else self.worker_label
        self.token = self.client.mint_worker_token(
            self.group,
            tok_name[:32],
            hard_usd_cap=self.hard_usd_cap,
            tpm_cap=self.tpm_cap,
            models=self.models,
            failover_policy=self.failover_policy,
            cache_mode=self.cache_mode,
            run_id=self.run_id,
            room_id=self.room_id,
            member_id=self.worker_label,
            task_id=self.task_id,
        )
        return self

    def __exit__(self, exc_type, exc, tb) -> bool:
        if self.token is not None:
            try:
                self.revoked = self.client.revoke_token(self.group, self.token.token_id)
            except Exception:
                self.revoked = False
            try:
                self.ledger = self.client.fetch_ledger(
                    group=self.group,
                    token_id=self.token.token_id,
                    run_id=self.run_id,
                )
            except Exception:
                self.ledger = None
        return False

    @property
    def host_cost_usd(self) -> float | None:
        if not isinstance(self.ledger, dict):
            return None
        totals = self.ledger.get("totals")
        if not isinstance(totals, dict) or "cost_usd" not in totals:
            return None
        return float(totals["cost_usd"])

    def env_overrides(self) -> dict[str, str]:
        """Unified provider env overrides routing Hermes, Devin, Claude Code, Codex, and OpenCode
        through `playbox-aiproxy` without per-harness OAuth or vendor key pools."""
        if self.token is None:
            raise RuntimeError("WorkerTokenLease must be entered before calling env_overrides()")
        base = self.client.base_url
        key = self.token.key
        custom_hdr_lines = [
            f"X-Playbox-Member-Id: {self.worker_label}",
            f"X-Playbox-Cache-Mode: {self.cache_mode}",
            f"X-Playbox-Failover-Policy: {self.failover_policy}",
        ]
        if self.run_id:
            custom_hdr_lines.append(f"X-Playbox-Run-Id: {self.run_id}")
        if self.room_id:
            custom_hdr_lines.append(f"X-Playbox-Room-Id: {self.room_id}")
        if self.task_id:
            custom_hdr_lines.append(f"X-Playbox-Task-Id: {self.task_id}")
        return {
            "PLAYBOX_AIPROXY_URL": base,
            "ANTHROPIC_BASE_URL": base,
            "ANTHROPIC_API_KEY": key,
            "ANTHROPIC_CUSTOM_HEADERS": "\n".join(custom_hdr_lines),
            "OPENAI_BASE_URL": f"{base}/v1",
            "OPENAI_API_BASE": f"{base}/v1",
            "OPENAI_API_KEY": key,
            "CUSTOM_BASE_URL": f"{base}/v1",
            "CUSTOM_API_KEY": key,
            "GEMINI_API_BASE": base,
            "GOOGLE_GEMINI_BASE_URL": base,
            "GEMINI_API_KEY": key,
            "X_PLAYBOX_RUN_ID": self.run_id,
            "X_PLAYBOX_MEMBER_ID": self.worker_label,
            "X_PLAYBOX_CACHE_MODE": self.cache_mode,
            "X_PLAYBOX_FAILOVER_POLICY": self.failover_policy,
        }

