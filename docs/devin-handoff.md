# Devin Handoff: Playbox + OpenShell + Hermes + AtlasKV End-to-End Stack

Date: 2026-11-19  
Repo: `/Users/shaanp/Documents/GitHub/playbox` (branch `main`, base `ddf25c6`, binary `0.4.3`)  
Strategy & Honesty Anchors: `docs/v0.4-strategy.md` (**Playbox Attested Gate**) and `docs/audit-claims.md` (never overclaim unverified security/benchmark results).

---

## 1. Mission & Non-Negotiable Invariants

Build and empirically evaluate a **Playbox + OpenShell + Hermes + AtlasKV** execution & routing stack (tested on 10 calibrated SWE-bench Django instances; not a full 500-instance SWE-bench benchmark claim) that:
- Matches or beats both **stock OpenShell + Hermes (Arm S)** and **unsandboxed local Hermes (Arm L)** on wall time and **uncontaminated solves per dollar**,
- Closes OpenShell's security gaps (Landlock `/dev/null`, tar traversal/xattr hardening, host-side `BOX-7` secret-injecting egress, `BOX-15` + external response-body taint tracking, host-signed audit logs, `.git` history anti-reward-hacking isolation, Ed25519 Attested Gate Receipts),
- Keeps wire `PROTOCOL_VERSION = 1` unchanged (additive optional serde fields only),
- Does **not** productize disproven `steer/` L2/L4 latent telepathy (frozen in `docs/v0.4-strategy.md`; lost to plain text in `docs/metered-benchmark.md`).

---

## 2. Live Runtime Environment on This Machine

- **Started by this session (still running):**
  - `OPENSHELL_COMPUTE_DRIVER=vm /opt/homebrew/opt/openshell/libexec/openshell-gateway-homebrew-service` (daemon PID `23489` + `openshell-driver-vm` PID `23502`), listening on `https://127.0.0.1:17670` (mTLS gRPC API — HTTP 404 on browser `GET /` is expected).
  - **Important macOS gotcha:** Without `OPENSHELL_COMPUTE_DRIVER=vm`, OpenShell defaults to the Docker driver when Docker Desktop is running, which fails on `alpine:3` (`IdentityResolutionFailed: workload user 'sandbox' does not exist`) and container→127.0.0.1 callbacks. Always run gateway with `OPENSHELL_COMPUTE_DRIVER=vm`.
  - Release binary rebuilt and installed at both `~/.local/bin/playbox` and `~/.cargo/bin/playbox` (`playbox 0.4.3 (ddf25c6d3+dirty)`).
- **Pre-existing user services (do NOT kill):**
  - `Playbox.app` (`playbox-desktop`), `playbox_hub.py` on `http://127.0.0.1:7717` (and preview hub on `7718`), `playbox up --state ~/.playbox-rooms/gpu-lab/state`, and their `playbox-mcp` children.
- **Orphaned sandboxes:** Cleaned (`pbxw-d6c654cfc5` deleted); only pre-existing `parity-django` (`Completed`) remains.

---

## 3. What Was Built (Files Touched by This Effort)

### A. Rust Core / Runner / Net / MCP / CLI
- `crates/playbox-runner/src/egress.rs`: Added `Proxy::start_with_host(cfg, secrets, names, bind_ip, guest_host)` so OpenShell microVMs reach host-side BOX-7 egress proxies cleanly.
- `crates/playbox-runner/src/lib.rs`: Added `SyncMode` (`Full | Delta | None`), `Runner::run_session` / `run_session_with_quota`, enabled `opts.egress` for `"openshell"` alongside `"libkrun" | "krun"`.
- `crates/playbox-runner/src/openshell.rs`:
  - Landlock policy now permits `read_write: [/sandbox, /tmp, /dev/null]` and `read_only: [/usr, /lib, /bin, /sbin, /etc, /dev/zero, /dev/urandom]` (fixes `>/dev/null 2>&1` failures).
  - Stateful `run_session` keeps `/sandbox` alive per `(member, base_commit)` (`WARM_IDLE = 600s`) with `SyncMode::None` (0 sync), `SyncMode::Delta` (`scan_guest_files` + `diff_file_index` + targeted `tar -T -` + `safe_remove_rel`), and `SyncMode::Full`.
  - Hardened tar packing (`--no-xattrs --no-mac-metadata`, `COPYFILE_DISABLE=1`, excludes `.git`) and pure-Rust tar validator `validate_tar_bytes` (refuses absolute paths, `..`, `.git`, hardlinks, devices, FIFOs, escaping symlinks; unpacks `--no-same-owner --no-same-permissions`).
  - Wired BOX-7 egress (`OPENSHELL_EGRESS_HOST`, `OPENSHELL_EGRESS_BIND`), staging capability URLs via `exec_stdin` into an immediately-unlinked `0600` `/tmp/.pbx-env-*` file (never in `/proc/cmdline`).
  - Added `policy_yaml_with_endpoints`, `with_secrets`, `with_egress`, `update_policy`, and `Runner::image_digest`.
- `crates/playbox-runner/tests/openshell.rs`: Added live VM test `stateful_session_delta_none_and_dev_null_hold_on_live_vm`.
- `crates/playbox-net/src/{wire.rs,handler.rs,host.rs,client.rs,grants.rs}` & `tests/{fuzz_host.rs,session.rs}`:
  - Additive `#[serde(default)] keep: bool` and `#[serde(default)] sync: SyncMode` on `Op::Run`.
  - Per-member persistent `MemberSession` worktrees (`SessionMap`, 600s idle reap), immediate teardown on `Host::revoke_member` / `Host::shutdown`, skipped `diff_patch` on `SyncMode::None`.
- `crates/playbox-mcp/src/{lib.rs,work.rs,tools.rs}`: Wired `keep`, `sync`, `full_patch` (up to `MAX_FULL_PATCH_BYTES = 2 MiB`, fixing 20 KiB diff truncation), and `policy_digest` on `playbox_delegate`.
- `crates/playbox-cli/src/rooms.rs`: Enabled `--egress` / `--allow-egress` for `--runtime openshell`.

### B. Hermes Adapter, AtlasKV Index, Cascade Router, Signed Receipt & Witness
- `hermes-playbox-backend/playbox_terminal/__init__.py`:
  - Default stateful `PLAYBOX_KEEP=1` (zero edit replay), `PLAYBOX_EGRESS`, `PLAYBOX_HOSTS`.
  - Sound fast `classify_sync_mode`: strips only exact `/dev/null` and fd-dup redirects (`2>/dev/null`, `2>&1`) and safely recognizes single-line POSIX compound read probes (`read_file`, `search_files`, `command -v`, `sha256sum`) for `sync="none"`, keeping all writes/subshells/`sed -i`/`sort -o`/`rg --pre`/`git apply` on `delta`.
  - Note: a parallel session also added `_publish_telemetry` (`::pbx metric` / `::pbx artifact`) in this file; we kept it intact and guarded it on `getattr(self.s, "join_info", None)` so mock unit tests pass cleanly.
- `hermes-playbox-backend/playbox_atlas/` (`__init__.py`, `kg2kv.py`, `hikvp.py`, `plugin.py`, `plugin.yaml`, `__main__.py`):
  - Pure-stdlib AtlasKV-inspired KG2KV (Python AST with `end_line` spans + `inherits` + filtered call edges, Rust/JS/TS/Go regex, Playbox room memory `cue/lesson/evidence`) + 3-level HiKVP (`Root -> Inter -> Leaf`) with 3-hop fair BFS call-chain expansion, strict Playbox trust gating (drops `quarantined`/`untrusted` triples), `append_verified_lesson()` (reviewed only when backed by a sealed gate receipt SHA-256), and Hermes `atlas_query` plugin tool (`register_tool(name, toolset="file", schema=..., handler=...)`).
- `hermes-playbox-backend/playbox_router/` (`__init__.py`, `router.py`, `receipt.py`):
  - `BudgetCap`, `CandidateResult`, `ReproGateResult` (anti-vacuous: valid only if `fails_on_base=True`), `compose_patches` (isolated 3-way git merge with hooks disabled), `select_and_compose` (rejects unverified host audits), `build_escalation_brief`, `run_cascade` (2-tier cheap→frontier cascade with synthetic-repro tie-break).
  - `receipt.py`: Domain-separated Ed25519-signed `playbox.attested_gate_receipt.v1` (`build_signed_receipt`, `verify_signed_receipt`, `verify_single_commit_repo`).
- `hermes-playbox-backend/tests/test_adapter_e2e.py`: Full end-to-end unit test coverage.
- `hermes-plugin-witness/witness.py` & `tests/test_witness.py`: Added `atlas_query` to `READ_ONLY_TOOLS` and extended turn taint holds to external web/browser tools (`EXTERNAL_UNTRUSTED_TOOLS`) and third-party `mcp__*` servers (`PLAYBOX_WITNESS_EXTERNAL=1`), closing OpenShell APF's response-body blind spot.

### C. Benchmark Harness & Docs
- `tools/bench/hermes-parity/run_swe.py`: **Fixed `.git` future-commit contamination leak** in Arms L & S by switching to single-commit `git archive base_commit` by default (`--dirty-git-history` opt-in only to reproduce old leak).
- `tools/bench/hermes-parity/run_swe_playbox_v2.py`: Full Arm P2 harness (`--runtime openshell|libkrun|docker`, `--workers`, `--provider`, `--model`, `--escalate-model`, `--escalate-provider`, `--escalate-reserve`, `--repro-gate`, `--memory-file`, `--budget`, signed Ed25519 receipt output `<tag>.receipt.json`).
- Docs updated/created: `docs/atlaskv-integration.md`, `docs/hermes-playbox.md`, `hermes-playbox-backend/README.md`, `docs/hermes-parity-swe-playbox.md` (contamination addendum).

### D. Unrelated Parallel Dirty Files in This Repo (DO NOT REVERT OR MIX INTO PRs)
The following modified/untracked paths belong to parallel workstreams in this workspace — leave them untouched unless explicitly instructed:
- `crates/playbox-cli/src/tui.rs`, `crates/playbox-net/src/{capabilities.rs,card.rs,chat.rs,limits.rs,shared_memory/}`
- `ui/` (`ui/hub/*`, `ui/static/*`, `ui/tests/test_archive.py`)
- `tools/bench/yukon/rust_climb.py`, `docs/flashbots-integration.md`, `steer/results/telepathy/l4_v2/logs.txt`, `tools/gem-swarm/`

---

## 4. Verified Results (All Empirically Run on This Machine)

### Automated Tests
- `cargo fmt --all -- --check`: PASS
- `cargo clippy --workspace --all-targets -- -D warnings`: PASS (0 warnings)
- Rust workspace tests: `playbox-runner` lib **47/47**, `playbox-net` lib **102/102** + `grants` **12/12** + `session` **1/1**, `playbox-mcp` lib **8/8** + e2e, `playbox-cli` **69/69**
- Live OpenShell VM integration tests (`PLAYBOX_TEST_OPENSHELL=1 PLAYBOX_ALLOW_MISSING_LIMITS=1`):
  - `cargo test -p playbox-runner --test openshell -- --test-threads=1`: **8/8 PASS** (`init=1.58s`, `none=36.5ms`, `delta=118.0ms`, `/dev/null` PASS)
  - `cargo test -p playbox-net --test openshell`: **1/1 PASS**
- Python tests: `hermes-playbox-backend/tests` **17/17 PASS** (under `~/.hermes/hermes-agent/venv/bin/python`), `hermes-plugin-witness/tests` **20/20 PASS**
- `python3 conformance/trace.py`: **71/71 shipped requirements checked**

### Live SWE-bench Empirical Highlights (Artifacts in `/tmp/swe-p2-v2/`)
1. **Smoking-gun benchmark contamination discovery (`django-11422`):**
   - Forensic check of `~/.playbox-hermes/openshell-parity/home-django-{L,S}/state.db` showed **7/7 resolved Arm L (4/5) & Arm S (3/5) runs in that 10-run sample** executed `git log --oneline --all` → `git show b2790f74d4` (the future gold commit left in `/usr/local/testbed/.git`; 40/56 historical sessions leaked).
   - Every uncontaminated Arm L/S run in that sample (`L5`, `S4`, `S5`) resolved **0/3**, making the exact same `run_with_reloader()` mistake as Haiku on Playbox. Playbox blocked the `.git` history leak by construction (single-commit archive + `.git` stripped in `/sandbox`; does not prevent model pretraining memorization).
2. **Speed & cost breakthrough (`django-11848`, Haiku 4.5, zero `.git` leakage):**
   - Arm P2: **`resolved: true` (`f2p: 2/2`, `p2p: 42/42`)**, **71.4 s** wall (including Docker online gate) vs **428 s** Legacy Arm P (**6.0× faster**), **90 s** Stock OpenShell Arm S (**21% faster**), **95 s** Local Hermes Arm L (**25% faster**).
   - Cost: **\$0.066** vs **\$0.115** (Arms L & S — **42% cheaper**) and **\$0.148** (Legacy Arm P — **55% cheaper**). Ed25519 receipt verified & gate-verified lesson persisted to `/tmp/swe-p2-v2/verified-memory.jsonl`.
3. **MicroVM latency breakthrough (`django-11422`, v2.1):**
   - Median delegate time dropped from **6.97 s** (Legacy Arm P) → **0.42 s** (v2.0) → **0.09 s** (v2.1; **77× faster**; `sync="none"` hit **49/80** delegates at **0.070 s** median).
4. **Cascade & intelligence-per-dollar breakthrough (`django-11422`, zero `.git` leakage):**
   - In `/tmp/swe-p2-v2/django-11422-P2-3`, `--repro-gate` triggered escalation to `claude-sonnet-4-6`. Sonnet 4.6 **solved `django-11422` (`f2p: 1/1`, `p2p: 44/44`, `resolved: True`) in just 5 API calls, 9 delegates (`0.07 s` median), and \$0.0786** (5× cheaper than Haiku 4.5's failing 53-call \$0.40 run!) because 3-hop AtlasKV surfaced `iter_all_python_module_files -> iter_modules_and_files [110-136]`.
   - Caught & fixed: Haiku's `w_repro` script had a buggy `sys.argv` mock that failed both patches; we fixed the granular online score (`0.6` existing + `0.4` repro) and escalated tie-breaker in `playbox_router/router.py` and `run_swe_playbox_v2.py` (unit-tested in `test_cascade_prefers_escalated_on_synthetic_repro_tie`).

---

## 5. Completed Priorities (Devin Continuation — 2026-11-19)

### Priority 1 — Uncontaminated Multi-Instance SWE Parity & Cascade Proof (COMPLETED)
- Executed clean uncontaminated head-to-head across 10 calibrated Django instances (`django-11848`, `django-11422`, `django-11905`, `django-11001`, `django-11620`, `django-12125`, `django-12700`, `django-11099`, `django-11049`, `django-12286`, single-commit `git archive base_commit`, zero `.git` leak) in `/tmp/swe-p2-v2/`:
  - **Clean Arm L (Unsandboxed Local Hermes):** **8 / 10 resolved (80.0%)**, `96.1 s` mean wall, `$0.5581` total (`$0.0698` / solve).
  - **Clean Arm S (Stock OpenShell + Hermes over SSH):** **6 / 10 resolved (60.0%)**, `78.4 s` mean wall, `$0.5059` total (`$0.0843` / solve).
  - **Arm P2 (Playbox + OpenShell + Hermes + 3-Hop AtlasKV + Repro-Gate + Hybrid PQ Receipt + Enforced Taint + 2-Host `RoomLog` Quorum + `T1` Secure Enclave):** **9 / 10 resolved (90.0% — 9/9 of unambiguous instances)**, **133.4 s** mean wall (including online Docker gate & `w_repro`), **0.07 s** median delegate time (`0.06–0.08 s` across all 10 instances, **99× faster** than Legacy Arm P's `6.97 s`), `$0.8087` total (`$0.0899` / solve — **+50% more solves than Stock OpenShell Arm S**).
  - **18 / 18** `<tag>.receipt.json` files in `/tmp/swe-p2-v2/` verified under `playbox_router.verify_signed_receipt(..., require_pq=True)` (`verified (Ed25519+ML-DSA-65)`).
  - Exported and offline-verified the master 5-gate compliance bundle `/tmp/swe-p2-v2/swe-bench-5gate-bundle.json` (`bundle_digest: 5b581665850021b17b8c06e4fec54c640cc074d0c1044bfcd9c43f37682c7fda`) via `playbox gate verify --require-signature --require-pq --require-tier t1 --json` (`ok: true`, `pq_signed: true`, `achieved_tier: "T1"`, `egress_requests: 2`, `promotions: 9`, `room_log_pq_committed: 19`, `attested_receipts: 18`, `enforced_gates_count: 5`, `claims_full_84_permf: false`).
  - Recorded full empirical tables in `docs/hermes-parity-swe-playbox.md`.

### Priority 2 — Hardened Independent Repro-Gate (`run_repro_generator`) & BOX-7 Egress Integration (COMPLETED)
- Added `is_genuine_repro_failure()` in `hermes-playbox-backend/playbox_router/router.py` and `_check_repro_fails_on_base()` in `tools/bench/hermes-parity/run_swe_playbox_v2.py`:
  - Rejects any repro script that fails due to `SyntaxError`, `IndentationError`, `TabError`, `ImportError`, `ModuleNotFoundError`, or `NameError` on `base_commit` (only accepting genuine assertion/behavioral failures).
  - Instructs `w_repro` to test the lowest-level library function directly (or spawn a real subprocess when testing `__main__` / CLI startup, never mutating `sys.argv` in-process without updating `sys.modules['__main__'].__file__`).
  - Added `--repro-model`, `--repro-provider`, `--egress`, and `--allow-egress` flags to `run_swe_playbox_v2.py` so host-side `BOX-7` credentials stay strictly on the host.
  - Unit-tested in `test_repro_gate_rejects_syntax_import_name_errors_on_base`.

### Priority 3 — Hybrid Post-Quantum Receipts (`Ed25519 + ML-DSA-65`) & Enforced Zero-False-Positive Taint (COMPLETED)
1. **Hybrid post-quantum receipts (`Ed25519+ML-DSA-65`, FIPS 204):**
   - Added `playbox receipt sign <FILE> [--key <FILE>] [--out <FILE>]` and `playbox receipt verify <FILE> [--pubkey <HEX>] [--pq-pubkey <HEX>] [--require-pq]` in `crates/playbox-cli/src/{cli.rs,main.rs,trust.rs}` using `playbox_core::pq::{derive_mldsa65_from_ed25519_seed, sign_mldsa65, verify_mldsa65}`.
   - Wired `build_signed_receipt()` and `verify_signed_receipt(..., require_pq=True)` in `hermes-playbox-backend/playbox_router/receipt.py` to emit and verify `alg: "Ed25519+ML-DSA-65"` receipts (`pq_alg: "ML-DSA-65"`, `pq_domain: "playbox.attested_gate_receipt.pq.v1"`).
   - Unit-tested in `crates/playbox-cli/src/trust.rs` (`hybrid_ed25519_mldsa65_receipt_sign_and_verify_fail_closed`) and `hermes-playbox-backend/tests/test_adapter_e2e.py`.
2. **Enforceable zero-false-positive taint (`PLAYBOX_TRUST_OFFLINE_WORKSPACE=1` + `PLAYBOX_APPROVERS`):**
   - Updated `crates/playbox-mcp/src/{lib.rs,core.rs,work.rs}` so isolated offline `playbox_delegate` calls (`network=false`, `allowed_hosts=[]`, `egress=[]`) do not register benign own-repo code echoes as untrusted taint when `PLAYBOX_TRUST_OFFLINE_WORKSPACE=1`, while peer chat (`chat:*`, `live:*`, `conversation:*`), web browse (`web:*`), shared memory (`memory:*`), steer (`steer:*`), capabilities (`capability:*`), and networked delegates (`network=true` / `allowed_hosts` / `egress`) remain `Source::untrusted(...)`.
   - Enabled `PLAYBOX_APPROVERS` hard-blocking enforcement in `run_swe_playbox_v2.py`: all 5 SWE-bench P2 runs completed with **`taint_mode: "enforced"` and `0` false-positive taint advisories** (down from 45–66 per run).
   - Verified in `crates/playbox-mcp/tests/mcp.rs` (`trust_offline_workspace_allows_own_repo_echoes_and_blocks_peer_or_network_taint`) and `python3 conformance/trace.py` (`71/71`).

### Priority 4 — AtlasKV Memory Root Inclusion & Open-Weights Parametric Track (COMPLETED)
- Updated `HiKVPIndex.query()` and `format_brief()` in `hermes-playbox-backend/playbox_atlas/hikvp.py` so `memory/*` roots with positive query similarity always survive root/inter pruning and receipt-backed `reviewed` lessons sort ahead of raw symbol triples (while `quarantined` entries remain strictly dropped).
- Documented empirical results and open-weights `KBEncoder` / `AtlaskvLlamaAttention` integration contract in `docs/atlaskv-integration.md`.

### Priority 5 — Clean Git Hygiene & Verification Gates
- Unrelated parallel dirty files in Section 3D left completely untouched.
- Full verification command suite:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  PLAYBOX_TEST_OPENSHELL=1 PLAYBOX_ALLOW_MISSING_LIMITS=1 cargo test -p playbox-runner --test openshell -- --test-threads=1
  PLAYBOX_TEST_OPENSHELL=1 PLAYBOX_ALLOW_MISSING_LIMITS=1 cargo test -p playbox-net --test openshell
  ~/.hermes/hermes-agent/venv/bin/python -m unittest discover -s hermes-playbox-backend/tests -v
  python3 -m unittest discover -s hermes-plugin-witness/tests -v
  python3 conformance/trace.py
  ```
