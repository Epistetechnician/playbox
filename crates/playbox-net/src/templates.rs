//! Built-in room templates (`swe`, `research`, `review`, `ops`) for one-command governed workspaces.

use crate::capabilities::Capability;
use crate::limits::AbuseLimits;
use crate::Host;
use playbox_runner::Limits;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TemplateKind {
    Swe,
    Research,
    Review,
    Ops,
    Hillclimb,
    #[serde(rename = "arc-rsi")]
    ArcRsi,
    #[serde(rename = "pqai-audit")]
    PqaiAudit,
    #[serde(rename = "pqai-migrate")]
    PqaiMigrate,
}

#[derive(Debug, Clone)]
pub struct TemplateSpec {
    pub kind: TemplateKind,
    pub id: &'static str,
    pub label: &'static str,
    pub summary: &'static str,
    pub default_runtime: &'static str,
    pub limits: Limits,
    pub abuse: AbuseLimits,
    pub memory_review: bool,
    pub capabilities: Vec<Capability>,
    pub canvas_seed: Vec<(&'static str, Value)>,
    pub starter_fact: (&'static str, &'static str),
}

impl TemplateKind {
    pub const ALL: &[TemplateKind] = &[
        TemplateKind::Swe,
        TemplateKind::Research,
        TemplateKind::Review,
        TemplateKind::Ops,
        TemplateKind::Hillclimb,
        TemplateKind::ArcRsi,
        TemplateKind::PqaiAudit,
        TemplateKind::PqaiMigrate,
    ];

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "swe" => Some(Self::Swe),
            "research" => Some(Self::Research),
            "review" => Some(Self::Review),
            "ops" => Some(Self::Ops),
            "hillclimb" | "hill-climb" => Some(Self::Hillclimb),
            "arc-rsi" | "arc_rsi" | "arcrsi" => Some(Self::ArcRsi),
            "pqai-audit" | "pqai_audit" | "pqaiaudit" => Some(Self::PqaiAudit),
            "pqai-migrate" | "pqai_migrate" | "pqaimigrate" => Some(Self::PqaiMigrate),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Swe => "swe",
            Self::Research => "research",
            Self::Review => "review",
            Self::Ops => "ops",
            Self::Hillclimb => "hillclimb",
            Self::ArcRsi => "arc-rsi",
            Self::PqaiAudit => "pqai-audit",
            Self::PqaiMigrate => "pqai-migrate",
        }
    }

    #[must_use]
    pub fn spec(self) -> TemplateSpec {
        let cap = |name: &str, argv: &[&str], approval: bool, timeout_secs: u64| Capability {
            name: name.to_string(),
            argv: argv.iter().map(|s| (*s).to_string()).collect(),
            params: BTreeMap::new(),
            approval,
            image: None,
            timeout_secs: Some(timeout_secs),
            mem_mib: None,
            mount_repo: false,
        };
        match self {
            Self::Swe => TemplateSpec {
                kind: self,
                id: "swe",
                label: "SWE · Multi-Agent Coding",
                summary: "Sandboxed coding & test runner, package-registry egress allowlist, diff replay, and handoff board.",
                default_runtime: "docker",
                limits: Limits {
                    cpus: 2.0,
                    mem_mib: 2048,
                    timeout_secs: 600,
                    network: false,
                    allowed_hosts: vec![
                        "github.com".into(),
                        "api.github.com".into(),
                        "crates.io".into(),
                        "index.crates.io".into(),
                        "pypi.org".into(),
                        "files.pythonhosted.org".into(),
                        "registry.npmjs.org".into(),
                    ],
                    allowed_secrets: Vec::new(),
                    allowed_egress: Vec::new(),
                },
                abuse: AbuseLimits {
                    max_members: 16,
                    max_concurrent_per_member: 4,
                    max_tasks_per_window: 30,
                    task_budget: 500,
                    ..AbuseLimits::default()
                },
                memory_review: false,
                capabilities: vec![
                    cap(
                        "test_suite",
                        &[
                            "sh",
                            "-c",
                            "if [ -f Cargo.toml ]; then cargo test --quiet; elif [ -f pyproject.toml ] || [ -f pytest.ini ]; then pytest -q; elif [ -f package.json ]; then npm test --silent; else echo 'no test suite detected'; fi",
                        ],
                        false,
                        300,
                    ),
                    cap("git_status", &["git", "status", "-sb"], false, 30),
                ],
                canvas_seed: vec![
                    (
                        "artifact",
                        json!({
                            "id": "swe-charter",
                            "title": "SWE Workspace Protocol",
                            "kind": "doc",
                            "status": "verified",
                            "summary": "Diff-first multi-agent software engineering workflow with verifiable replay and structured handoffs.",
                            "body": "# SWE Room Protocol\n\n1. Pull `main` and run tasks in the sandbox via `playbox_delegate`.\n2. Record verified codebase facts with `playbox_room_remember` (`task_id`, `commit`).\n3. Verify reproducibility with `playbox_replay` before handing off via `playbox_handoff`."
                        }),
                    ),
                    (
                        "card",
                        json!({"id": "swe-1", "title": "Reproduce & isolate target issue", "col": "todo", "note": "Run baseline check in sandbox and record task_id"}),
                    ),
                    (
                        "card",
                        json!({"id": "swe-2", "title": "Implement patch & run tests", "col": "todo", "note": "Keep changes minimal and verify diff"}),
                    ),
                    (
                        "card",
                        json!({"id": "swe-3", "title": "Replay & publish handoff checkpoint", "col": "todo", "note": "Run playbox_replay and playbox_handoff"}),
                    ),
                ],
                starter_fact: (
                    "swe-protocol",
                    "Template: SWE multi-agent workspace. Every code change should be verified with playbox_replay and handed off via playbox_handoff.",
                ),
            },
            Self::Research => TemplateSpec {
                kind: self,
                id: "research",
                label: "Research · Empirical & ML",
                summary: "High-budget compute limits (4 CPU / 4 GiB), ML hub allowlist, live metric tracking, and reproducible evaluation.",
                default_runtime: "docker",
                limits: Limits {
                    cpus: 4.0,
                    mem_mib: 4096,
                    timeout_secs: 1800,
                    network: false,
                    allowed_hosts: vec![
                        "huggingface.co".into(),
                        "cdn-lfs.huggingface.co".into(),
                        "pypi.org".into(),
                        "files.pythonhosted.org".into(),
                        "arxiv.org".into(),
                    ],
                    allowed_secrets: Vec::new(),
                    allowed_egress: Vec::new(),
                },
                abuse: AbuseLimits {
                    max_members: 24,
                    max_concurrent_per_member: 4,
                    max_tasks_per_window: 40,
                    task_budget: 1000,
                    ..AbuseLimits::default()
                },
                memory_review: false,
                capabilities: vec![
                    cap(
                        "run_benchmark",
                        &[
                            "sh",
                            "-c",
                            "python3 -m pytest -q || echo 'benchmark runner ready'",
                        ],
                        false,
                        600,
                    ),
                    cap("promote_candidate", &["git", "rev-parse", "HEAD"], true, 60),
                ],
                canvas_seed: vec![
                    (
                        "artifact",
                        json!({
                            "id": "research-protocol",
                            "title": "Empirical Research Protocol",
                            "kind": "doc",
                            "status": "verified",
                            "summary": "Hypothesis-driven experimentation with reproducible runs and holdout verification.",
                            "body": "# Research Room Protocol\n\n1. Log baseline metrics with `::pbx metric`.\n2. Attach `task_id` and `commit` to every finding in Room Memory.\n3. Replay winning runs with `playbox_replay` to prove determinism."
                        }),
                    ),
                    (
                        "card",
                        json!({"id": "res-1", "title": "Establish baseline metric", "col": "todo", "note": "Run baseline harness and record score"}),
                    ),
                    (
                        "card",
                        json!({"id": "res-2", "title": "Run ablation experiments", "col": "todo", "note": "Compare candidate patches against baseline"}),
                    ),
                    (
                        "card",
                        json!({"id": "res-3", "title": "Verify winning run via playbox_replay", "col": "todo", "note": "Confirm identical patch_hash and policy_digest"}),
                    ),
                ],
                starter_fact: (
                    "research-protocol",
                    "Template: Empirical research workspace. Attach task_id and commit provenance to all benchmark findings in Room Memory.",
                ),
            },
            Self::Review => TemplateSpec {
                kind: self,
                id: "review",
                label: "Review · Security & Code Audit",
                summary: "Zero-egress audit sandbox, owner-gated memory review, secret scanning capability, and deterministic diff verification.",
                default_runtime: "docker",
                limits: Limits {
                    cpus: 1.0,
                    mem_mib: 1024,
                    timeout_secs: 180,
                    network: false,
                    allowed_hosts: Vec::new(),
                    allowed_secrets: Vec::new(),
                    allowed_egress: Vec::new(),
                },
                abuse: AbuseLimits {
                    max_members: 12,
                    max_concurrent_per_member: 2,
                    max_tasks_per_window: 20,
                    task_budget: 200,
                    ..AbuseLimits::default()
                },
                memory_review: true,
                capabilities: vec![
                    cap(
                        "secret_scan",
                        &[
                            "sh",
                            "-c",
                            "grep -rIEn 'AKIA[0-9A-Z]{16}|BEGIN [A-Z ]*PRIVATE KEY' . 2>/dev/null | head -n 20 || true",
                        ],
                        false,
                        60,
                    ),
                    cap(
                        "approve_release",
                        &["git", "log", "-n", "5", "--oneline"],
                        true,
                        30,
                    ),
                ],
                canvas_seed: vec![
                    (
                        "artifact",
                        json!({
                            "id": "review-checklist",
                            "title": "Security & Code Review Checklist",
                            "kind": "proof",
                            "status": "checking",
                            "summary": "Zero-egress review sandbox with owner-reviewed memory and replay verification.",
                            "body": "# Review Checklist\n\n- [ ] Inspect task patches and replay reproducibility\n- [ ] Audit taint and provenance of shared memory facts\n- [ ] Verify no secrets or unauthorized egress"
                        }),
                    ),
                    (
                        "card",
                        json!({"id": "rev-1", "title": "Inspect candidate diff & replay task", "col": "todo", "note": "Use playbox_replay to verify patch hash"}),
                    ),
                    (
                        "card",
                        json!({"id": "rev-2", "title": "Audit security & taint boundaries", "col": "todo", "note": "Review pending Room Memory facts"}),
                    ),
                    (
                        "card",
                        json!({"id": "rev-3", "title": "Sign off release artifact", "col": "todo", "note": "Move checklist artifact to verified"}),
                    ),
                ],
                starter_fact: (
                    "review-policy",
                    "Template: Security & Code Review room. Network egress is disabled and guest memory entries wait for owner review.",
                ),
            },
            Self::Ops => TemplateSpec {
                kind: self,
                id: "ops",
                label: "Ops · Incident & Runbook",
                summary: "Diagnostics + owner-approved remediation capabilities, curated status egress, and shift handoff checkpoints.",
                default_runtime: "docker",
                limits: Limits {
                    cpus: 2.0,
                    mem_mib: 2048,
                    timeout_secs: 300,
                    network: false,
                    allowed_hosts: vec!["status.github.com".into(), "api.github.com".into()],
                    allowed_secrets: Vec::new(),
                    allowed_egress: Vec::new(),
                },
                abuse: AbuseLimits {
                    max_members: 16,
                    max_concurrent_per_member: 2,
                    max_tasks_per_window: 20,
                    task_budget: 300,
                    ..AbuseLimits::default()
                },
                memory_review: true,
                capabilities: vec![
                    cap(
                        "health_check",
                        &["sh", "-c", "uname -a && df -h ."],
                        false,
                        30,
                    ),
                    cap(
                        "remediate",
                        &["sh", "-c", "echo 'remediation executed'"],
                        true,
                        120,
                    ),
                ],
                canvas_seed: vec![
                    (
                        "artifact",
                        json!({
                            "id": "ops-runbook",
                            "title": "Incident & Operations Runbook",
                            "kind": "doc",
                            "status": "verified",
                            "summary": "Governed ops workspace with owner-approved remediation capabilities and audit trail.",
                            "body": "# Ops Runbook\n\n1. Run read-only diagnostics first.\n2. Remediation capabilities require owner passkey step-up approval.\n3. Post structured handoff checkpoints between on-call shifts."
                        }),
                    ),
                    (
                        "card",
                        json!({"id": "ops-1", "title": "Triage & collect diagnostics", "col": "todo", "note": "Run health_check capability"}),
                    ),
                    (
                        "card",
                        json!({"id": "ops-2", "title": "Execute approved remediation", "col": "todo", "note": "Requires owner approval"}),
                    ),
                    (
                        "card",
                        json!({"id": "ops-3", "title": "Record post-incident facts & handoff", "col": "todo", "note": "Endorse verified root cause in Room Memory"}),
                    ),
                ],
                starter_fact: (
                    "ops-runbook",
                    "Template: Operations & Incident room. Remediation capabilities require owner approval; record verified root causes in Room Memory.",
                ),
            },
            Self::Hillclimb => TemplateSpec {
                kind: self,
                id: "hillclimb",
                label: "Hillclimb · Autonomous Metric Optimization",
                summary: "Board-driven multi-agent optimization with signed Room Manifest, deterministic `/eval` capability receipts, and event-driven schedules.",
                default_runtime: "docker",
                limits: Limits {
                    cpus: 4.0,
                    mem_mib: 4096,
                    timeout_secs: 600,
                    network: false,
                    allowed_hosts: Vec::new(),
                    allowed_secrets: Vec::new(),
                    allowed_egress: Vec::new(),
                },
                abuse: AbuseLimits {
                    max_members: 16,
                    max_concurrent_per_member: 3,
                    max_tasks_per_window: 30,
                    task_budget: 1000,
                    ..AbuseLimits::default()
                },
                memory_review: false,
                capabilities: vec![cap("status", &["git", "status", "--short"], false, 30)],
                canvas_seed: vec![
                    (
                        "artifact",
                        json!({
                            "id": "hillclimb-protocol",
                            "title": "Hillclimb Room Protocol",
                            "kind": "doc",
                            "status": "verified",
                            "summary": "Autonomous multi-agent hill-climbing via signed Room Manifest, `/eval`, and unread mention queues.",
                            "body": "# Hillclimb Protocol\n\n1. Read the signed Room Manifest (`playbox_manifest`) for the goal, `/commands`, and budget.\n2. Pull the frontier, test a hypothesis, push to your branch, and invoke `/eval` in the feed.\n3. Record wins and failed hypotheses in Room Memory so teammates do not repeat dead ends."
                        }),
                    ),
                    (
                        "card",
                        json!({"id": "hc-1", "title": "Baseline evaluation", "col": "todo", "note": "Run `/eval` on main to establish baseline metric"}),
                    ),
                    (
                        "card",
                        json!({"id": "hc-2", "title": "Iterate frontier", "col": "todo", "note": "Push candidate improvements and verify via `/eval`"}),
                    ),
                ],
                starter_fact: (
                    "hillclimb-protocol",
                    "Template: Autonomous hill-climbing room. Inspect the signed Room Manifest for goal and `/commands`; post `/eval` to score candidates.",
                ),
            },
            Self::ArcRsi => TemplateSpec {
                kind: self,
                id: "arc-rsi",
                label: "ARC-RSI · Sovereign CPU-ARC + Maverick + Self-Rewrite",
                summary: "Zero-trust sovereign agent room combining CPU-ARC bytecode skill caching, Goldilocks Maverick Verification-Only, 64B Swarm Telepathy descriptors, and quorum-signed promotion gates.",
                default_runtime: "krun",
                limits: Limits {
                    cpus: 4.0,
                    mem_mib: 4096,
                    timeout_secs: 600,
                    network: false,
                    allowed_hosts: Vec::new(),
                    allowed_secrets: Vec::new(),
                    allowed_egress: Vec::new(),
                },
                abuse: AbuseLimits {
                    max_members: 16,
                    max_concurrent_per_member: 2,
                    max_tasks_per_window: 24,
                    task_budget: 1000,
                    ..AbuseLimits::default()
                },
                memory_review: true,
                capabilities: vec![
                    cap(
                        "arc-compile",
                        &["python3", "-m", "arc_membrane.cpu_arc_compiler"],
                        false,
                        120,
                    ),
                    cap(
                        "maverick-verify",
                        &["cargo", "test", "--lib", "maverick"],
                        false,
                        120,
                    ),
                    cap(
                        "rsi-eval",
                        &["python3", "-m", "arc_membrane.self_rewrite_bridge"],
                        false,
                        300,
                    ),
                    cap("promote", &["playbox", "gate", "verify"], true, 60),
                ],
                canvas_seed: vec![
                    (
                        "artifact",
                        json!({
                            "id": "arc-rsi-protocol",
                            "title": "ARC-RSI Sovereign Room Protocol",
                            "kind": "doc",
                            "status": "verified",
                            "summary": "BOX-7 fail-closed proxy + CPU-ARC hit cache + Maverick Verification-Only + 64B Swarm Telepathy + dual-signed Ed25519+ML-DSA-65 quorum promotions.",
                            "body": "# ARC-RSI Protocol\n\n1. All model egress passes through BOX-7 (`playbox-aiproxy`) with CPU-ARC local hit bypass and Maverick Goldilocks Verification-Only.\n2. Novel compiled skills broadcast 64-byte int8 Swarm Telepathy descriptors (`PBL2`) bound to signed receipts.\n3. Weight rewrites run router-frozen exact QR null-space projection and require quorum-signed (`j >= 2`) dual Ed25519+ML-DSA-65 promotion receipts; zero unsigned promotions are accepted."
                        }),
                    ),
                    (
                        "card",
                        json!({"id": "arc-1", "title": "Compile & certify CPU-ARC skills", "col": "todo", "note": "Run `/arc-compile` and broadcast novel 64B int8 descriptors"}),
                    ),
                    (
                        "card",
                        json!({"id": "arc-2", "title": "Verify Maverick Goldilocks matvec proofs", "col": "todo", "note": "Run `/maverick-verify` (1-RTT honest + cheat rejection)"}),
                    ),
                    (
                        "card",
                        json!({"id": "arc-3", "title": "Sealed F3 self-rewrite quorum promotion", "col": "todo", "note": "Run `/rsi-eval`, collect j>=2 host receipts, verify offline via `/promote`"}),
                    ),
                ],
                starter_fact: (
                    "arc-rsi-protocol",
                    "Template: ARC-RSI sovereign room. Zero unsigned promotions; all egress flows through BOX-7 CPU-ARC + Maverick Verification-Only with dual Ed25519+ML-DSA-65 receipts.",
                ),
            },
            Self::PqaiAudit => TemplateSpec {
                kind: self,
                id: "pqai-audit",
                label: "PQAI · Air-Gapped Security & Client Audit Room",
                summary: "Zero-egress microVM room for AI-assisted auditing & fuzzing of pq-cl clients, hash-based verifiers, and smart contracts with tri-signed SLH-DSA 5-gate bundles.",
                default_runtime: "krun",
                limits: Limits {
                    cpus: 4.0,
                    mem_mib: 4096,
                    timeout_secs: 900,
                    network: false,
                    allowed_hosts: Vec::new(),
                    allowed_secrets: Vec::new(),
                    allowed_egress: Vec::new(),
                },
                abuse: AbuseLimits {
                    max_members: 16,
                    max_concurrent_per_member: 3,
                    max_tasks_per_window: 30,
                    task_budget: 1000,
                    ..AbuseLimits::default()
                },
                memory_review: true,
                capabilities: vec![
                    cap(
                        "pqai-scan",
                        &["playbox", "pqai", "scan", ".", "--json"],
                        false,
                        120,
                    ),
                    cap("test_suite", &["cargo", "test"], false, 600),
                    cap(
                        "gate-verify-pqai",
                        &["playbox", "gate", "verify", "--require-pqai"],
                        true,
                        60,
                    ),
                ],
                canvas_seed: vec![
                    (
                        "artifact",
                        json!({
                            "id": "pqai-audit-charter",
                            "title": "PQAI Air-Gapped Audit Room Charter",
                            "kind": "doc",
                            "status": "verified",
                            "summary": "Zero-egress microVM + BOX-7 confinement + anti-Goodhart verified_loop EB+MDL gate + FIPS 205 SLH-DSA tri-signed 5-gate compliance bundles.",
                            "body": "# PQAI Audit Room\n\n1. Air-gapped execution (`krun` microVM, `network=false`, empty `allowed_egress`) prevents unpatched 0-day exfiltration.\n2. Run `/pqai-scan` to inventory classical assumptions (`ecrecover`, BLS12-381, BN254, KZG) and lattice/algebraic-hash dependencies.\n3. Patches promote only after deterministic test oracles, verified-loop trajectory hygiene + anytime EB+MDL LCB, and offline `/gate-verify-pqai` (`Ed25519 + ML-DSA-65 + SLH-DSA-SHA2-128f`)."
                        }),
                    ),
                    (
                        "card",
                        json!({"id": "pqai-1", "title": "Inventory classical & lattice assumptions", "col": "todo", "note": "Run `/pqai-scan` across client / contract targets"}),
                    ),
                    (
                        "card",
                        json!({"id": "pqai-2", "title": "Fuzz & formally verify critical paths offline", "col": "todo", "note": "Zero-egress microVM; record all findings in owner-reviewed Room Memory"}),
                    ),
                    (
                        "card",
                        json!({"id": "pqai-3", "title": "Export & verify tri-signed SLH-DSA 5-gate bundle", "col": "todo", "note": "Owner approval required via `/gate-verify-pqai` (`--require-pqai`)"}),
                    ),
                ],
                starter_fact: (
                    "pqai-audit-charter",
                    "Template: PQAI Air-Gapped Audit Room. Zero network egress (BOX-7 fail-closed), owner-gated Room Memory, and FIPS 205 SLH-DSA tri-signed 5-gate receipts required for promotion.",
                ),
            },
            Self::PqaiMigrate => TemplateSpec {
                kind: self,
                id: "pqai-migrate",
                label: "PQAI · Pre-Quantum to Frames & Hash-Crypto Migration",
                summary: "Governed air-gapped room that migrates ecrecover, BLS, and KZG codebases to Hegotá Frames, EIP-8288/8479, and FIPS 205 SLH-DSA with deterministic before/after receipts.",
                default_runtime: "krun",
                limits: Limits {
                    cpus: 4.0,
                    mem_mib: 4096,
                    timeout_secs: 900,
                    network: false,
                    allowed_hosts: Vec::new(),
                    allowed_secrets: Vec::new(),
                    allowed_egress: Vec::new(),
                },
                abuse: AbuseLimits {
                    max_members: 16,
                    max_concurrent_per_member: 3,
                    max_tasks_per_window: 30,
                    task_budget: 1000,
                    ..AbuseLimits::default()
                },
                memory_review: true,
                capabilities: vec![
                    cap(
                        "pqai-scan",
                        &["playbox", "pqai", "scan", ".", "--json"],
                        false,
                        120,
                    ),
                    cap(
                        "pqai-verify-migration",
                        &["playbox", "pqai", "verify-migration", "--json"],
                        false,
                        180,
                    ),
                    cap(
                        "gate-verify-pqai",
                        &["playbox", "gate", "verify", "--require-pqai"],
                        true,
                        60,
                    ),
                ],
                canvas_seed: vec![
                    (
                        "artifact",
                        json!({
                            "id": "pqai-migrate-runbook",
                            "title": "PQAI Pre-Quantum to Hash-Based Migration Runbook",
                            "kind": "doc",
                            "status": "verified",
                            "summary": "Deterministic migration from ecrecover/BLS/KZG to Hegotá Frames + hash-based signatures/commitments verified by anytime EB+MDL and SLH-DSA tri-signed receipts.",
                            "body": "# PQAI Migration Runbook\n\n1. Capture baseline inventory with `/pqai-scan` (`ecrecover`, BLS12-381/BN254, KZG `pointEvaluation`, classical custody).\n2. Migrate account verification to Hegotá Frames (`EIP-8141`) + hash-based verifiers (`SLH-DSA`/`XMSS`/`EIP-8288`) and DA checks to `EIP-8479` hash-based commitments.\n3. Verify strict reduction of pre-quantum findings + zero regressions + verified-loop trajectory hygiene via `/pqai-verify-migration` and seal with `--require-pqai`."
                        }),
                    ),
                    (
                        "card",
                        json!({"id": "mig-1", "title": "Baseline pre-quantum scan", "col": "todo", "note": "Run `/pqai-scan` on target repo before edits"}),
                    ),
                    (
                        "card",
                        json!({"id": "mig-2", "title": "Migrate ecrecover / BLS / KZG to Frames & hash-based verifiers", "col": "todo", "note": "Keep zero-egress microVM confinement active"}),
                    ),
                    (
                        "card",
                        json!({"id": "mig-3", "title": "Run deterministic migration verifier & emit SLH-DSA receipt", "col": "todo", "note": "Run `/pqai-verify-migration` and approve via `/gate-verify-pqai`"}),
                    ),
                ],
                starter_fact: (
                    "pqai-migrate-runbook",
                    "Template: PQAI Migration Room. Air-gapped microVM; every migration patch must strictly reduce pre-quantum findings with zero regressions and pass `--require-pqai` SLH-DSA verification.",
                ),
            },
        }
    }

    #[must_use]
    pub fn describe_all() -> Value {
        Value::Array(
            Self::ALL
                .iter()
                .map(|k| {
                    let s = k.spec();
                    json!({
                        "id": s.id,
                        "label": s.label,
                        "summary": s.summary,
                        "runtime": s.default_runtime,
                        "limits": {
                            "cpus": s.limits.cpus,
                            "mem_mib": s.limits.mem_mib,
                            "timeout_secs": s.limits.timeout_secs,
                            "network": s.limits.network,
                            "allowed_hosts": s.limits.allowed_hosts,
                        },
                        "budget": {
                            "task_budget": s.abuse.task_budget,
                            "max_members": s.abuse.max_members,
                            "max_concurrent": s.abuse.max_concurrent_per_member,
                            "max_tasks_per_minute": s.abuse.max_tasks_per_window,
                        },
                        "memory_review": s.memory_review,
                        "capabilities": s.capabilities.iter().map(|c| json!({
                            "name": c.name,
                            "approval": c.approval,
                            "timeout_secs": c.timeout_secs,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect(),
        )
    }
}

/// Seed a freshly created templated room with its starter Canvas board items and owner-endorsed protocol memory.
pub async fn seed_room(host: &Host, kind: TemplateKind) {
    let spec = kind.spec();
    let existing = host.feed(0);
    let already_seeded = existing.items.iter().any(|i| {
        i.kind == "say"
            && (i.text.starts_with("::pbx card ") || i.text.starts_with("::pbx artifact "))
    });
    if !already_seeded {
        for (pbx_kind, payload) in &spec.canvas_seed {
            let line = format!(
                "::pbx {pbx_kind} {}",
                crate::capabilities::canonical(payload)
            );
            let _ = host.say(&line, Some("host"));
        }
    }
    let (key, text) = spec.starter_fact;
    let _ = host
        .memory_action(&json!({
            "action": "remember",
            "key": key,
            "text": text,
            "endorsed": true,
        }))
        .await;
}
