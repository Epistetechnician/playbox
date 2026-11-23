import Playbox.Grants

/-!
# Playbox multi-agent taint confinement and non-interference

Formalizes the host-proxy taint latch and multi-agent delegation invariants enforced by
Playbox (`BOX-7` egress proxy + `playbox-aiproxy` + mutually attested OpenShell rooms).

1. **Monotonicity (`taint_monotonic`)**: Once a worker's taint latch trips on untrusted input
   (poisoned issue, external dependency, or prompt injection), no subsequent unprivileged step
   clears it.
2. **Fail-closed egress (`tainted_blocks_exfil`)**: A tainted worker can never authorize
   non-allowlisted outbound egress or secret-bearing requests.
3. **Memory / critic non-poisoning (`tainted_cannot_commit_memory`)**: A tainted worker cannot
   promote artifacts into shared room memory or bypass an independent critic gate without
   explicit host-attested declassification.
4. **Multi-agent non-interference (`multi_agent_confinement`)**: Across an
   Orchestrator -> Worker -> Critic delegation chain, grant attenuation (`Grants.lean`) and
   taint confinement compose: a compromised Worker neither widens its parent's grant nor
   exfiltrates secrets nor poisons the Critic/Orchestrator memory plane.
-/

namespace Playbox

/-- Security classification of an execution event observed by the host proxy. -/
inductive Event where
  | localRead
  | untrustedInput
  | toolCall
  deriving DecidableEq, Repr

/-- Outbound egress target class mediated by the host BOX-7 proxy. -/
inductive EgressKind where
  | allowlistedApi   -- e.g. host-attested playbox-aiproxy (no raw secret in VM)
  | externalHost     -- arbitrary internet / exfiltration sink
  | secretBearing    -- request carrying raw credentials or host env
  deriving DecidableEq, Repr

/-- Host-tracked confinement state for a single microVM worker in a Playbox room. -/
structure WorkerState where
  grant          : Grant
  tainted        : Bool
  declassified   : Bool

/-- Single-step host taint transition: untrusted input irreversibly sets `tainted := true`
and clears any prior declassification token. -/
def stepTaint (s : WorkerState) (ev : Event) : WorkerState :=
  match ev with
  | Event.untrustedInput => { s with tainted := true, declassified := false }
  | Event.localRead      => s
  | Event.toolCall       => s

/-- Trace transition over a sequence of events. -/
def runTrace (s : WorkerState) : List Event → WorkerState
  | [] => s
  | ev :: rest => runTrace (stepTaint s ev) rest

/-- Host BOX-7 egress authorization predicate:
- `externalHost` and `secretBearing` are always denied when `s.tainted = true`.
- `secretBearing` is unconditionally denied inside any governed microVM (secrets never enter the VM). -/
def egressAllowed (s : WorkerState) (revoked : List Nat) (now : Nat) (r : Req) (k : EgressKind) : Prop :=
  authorizes s.grant revoked now r ∧
  match k with
  | EgressKind.secretBearing  => False
  | EgressKind.externalHost   => s.tainted = false ∧ r.net > 0
  | EgressKind.allowlistedApi => True

/-- Shared room memory / critic promotion gate: a worker may promote a lesson or patch into
shared room memory only when untainted, or after explicit host-attested declassification. -/
def canCommitMemory (s : WorkerState) : Prop :=
  s.tainted = false ∨ s.declassified = true

/-- Theorem 1a: Single-step taint monotonicity. Once tainted, a worker remains tainted. -/
theorem stepTaint_monotonic (s : WorkerState) (ev : Event) (ht : s.tainted = true) :
    (stepTaint s ev).tainted = true := by
  cases ev <;> simp [stepTaint, ht]

/-- Theorem 1b: Trace taint monotonicity over any finite sequence of events. -/
theorem runTrace_taint_monotonic (s : WorkerState) (evs : List Event) (ht : s.tainted = true) :
    (runTrace s evs).tainted = true := by
  induction evs generalizing s with
  | nil => exact ht
  | cons ev rest ih => exact ih (stepTaint s ev) (stepTaint_monotonic s ev ht)

/-- Theorem 1c: Any trace containing `untrustedInput` leaves the worker tainted. -/
theorem untrusted_input_latches_taint (s : WorkerState) (pre post : List Event) :
    (runTrace s (pre ++ Event.untrustedInput :: post)).tainted = true := by
  induction pre generalizing s with
  | nil =>
      exact runTrace_taint_monotonic (stepTaint s Event.untrustedInput) post rfl
  | cons ev rest ih =>
      exact ih (stepTaint s ev)

/-- Theorem 2a: Fail-closed exfiltration prevention. A tainted worker can never authorize
external-host egress. -/
theorem tainted_blocks_external_egress {s : WorkerState} {revoked : List Nat} {now : Nat} {r : Req}
    (ht : s.tainted = true) : ¬ egressAllowed s revoked now r EgressKind.externalHost := by
  intro ⟨_, hnt, _⟩
  rw [ht] at hnt
  cases hnt

/-- Theorem 2b: Secrets never leave via any governed microVM regardless of taint state. -/
theorem governed_blocks_secret_exfil {s : WorkerState} {revoked : List Nat} {now : Nat} {r : Req} :
    ¬ egressAllowed s revoked now r EgressKind.secretBearing := by
  intro ⟨_, hfalse⟩
  exact hfalse

/-- Theorem 3: A tainted worker without explicit host declassification cannot poison
shared room memory or override the critic plane. -/
theorem tainted_cannot_poison_memory {s : WorkerState}
    (ht : s.tainted = true) (hnd : s.declassified = false) : ¬ canCommitMemory s := by
  intro h
  rcases h with hnt | hd
  · rw [ht] at hnt; cases hnt
  · rw [hnd] at hd; cases hd

/-- Theorem 3b: Encountering untrusted input immediately revokes memory-commit capability
until an explicit host-attested declassification occurs. -/
theorem untrusted_input_revokes_memory_commit (s : WorkerState) :
    ¬ canCommitMemory (stepTaint s Event.untrustedInput) := by
  exact tainted_cannot_poison_memory rfl rfl

/-- Multi-agent room configuration: Orchestrator -> Worker -> Critic with isolated microVM states. -/
structure MultiAgentRoom where
  orchestrator : WorkerState
  worker       : WorkerState
  critic       : WorkerState
  attenuated   : Grant.le worker.grant orchestrator.grant
  critic_clean : critic.tainted = false

/-- Theorem 4 (Multi-Agent Confinement & Non-Interference):
When a delegated Worker ingests poisoned input (`Event.untrustedInput`) and executes an arbitrary
follow-up trace `post`:
  (a) Every resource/command request the Worker can make stays within the Orchestrator's root grant.
  (b) The Worker is permanently unable to exfiltrate to any external host or leak secrets.
  (c) Without explicit host declassification, the Worker cannot commit to shared room memory.
  (d) The isolated Critic VM's taint bit remains untouched (`critic.tainted = false`). -/
theorem multi_agent_confinement (room : MultiAgentRoom) (post : List Event)
    (revoked : List Nat) (now : Nat) (r : Req) :
    let w' := runTrace (stepTaint room.worker Event.untrustedInput) post
    (withinLimits w'.grant now r → withinLimits room.orchestrator.grant now r) ∧
    (¬ egressAllowed w' revoked now r EgressKind.externalHost) ∧
    (¬ egressAllowed w' revoked now r EgressKind.secretBearing) ∧
    (w'.declassified = false → ¬ canCommitMemory w') ∧
    (room.critic.tainted = false) := by
  have hgrant : ∀ s evs, (runTrace s evs).grant = s.grant := by
    intro s evs
    induction evs generalizing s with
    | nil => rfl
    | cons ev rest ih =>
        have hg : (stepTaint s ev).grant = s.grant := by cases ev <;> rfl
        rw [runTrace, ih, hg]
  have ht : (runTrace (stepTaint room.worker Event.untrustedInput) post).tainted = true :=
    runTrace_taint_monotonic (stepTaint room.worker Event.untrustedInput) post rfl
  refine ⟨?_, ?_, ?_, ?_, room.critic_clean⟩
  · intro hw
    rw [hgrant] at hw
    exact withinLimits_mono room.attenuated hw
  · exact tainted_blocks_external_egress ht
  · exact governed_blocks_secret_exfil
  · intro hnd
    exact tainted_cannot_poison_memory ht hnd

end Playbox
