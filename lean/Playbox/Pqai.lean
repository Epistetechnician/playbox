import Playbox.Grants

/-!
# Playbox PQAI Root-of-Trust Invariants

Formalizes the two hash-only (`FIPS 205 SLH-DSA-SHA2-128f`) trust-root invariants
introduced in Layer 1:

1. **Biscuit root anchoring (`BiscuitRootAnchor`):** A valid hash-based anchor pins the
   room identity, Biscuit Ed25519 root key, expected SLH-DSA public key, and revocation
   set. Cross-room replay, unpinned SLH-DSA keys, invalid signatures, and revoked grants
   are all rejected fail-closed, and expanding the anchored revocation set across epochs
   can only shrink authorized requests.
2. **RoomLog tri-signed PQAI checkpoint safety (`pqai_count` / `committed_pqai`):**
   A PQAI cosignature strictly requires Ed25519, ML-DSA-65, and SLH-DSA-SHA2-128f
   (classical-only or lattice-only cosignatures never count). Under strict majority
   quorums (`2 * k > n`), any two conflicting PQAI-committed checkpoints share at least
   one host, guaranteeing safety when hosts sign at most one checkpoint per `(epoch, index)`.
-/

namespace Playbox.Pqai

/-- Abstract hash-anchored Biscuit root (`crates/playbox-net/src/grants.rs`). -/
structure RootAnchor where
  room          : String
  epoch         : Nat
  biscuitRootPk : Nat
  revoked       : List Nat
  slhPk         : Nat
  validHashSig  : Bool

/-- Fail-closed verification of a `BiscuitRootAnchor` against pinned room/root/SLH-DSA keys. -/
def validAnchor (expectedRoom : String) (expectedRootPk expectedSlhPk : Nat) (a : RootAnchor) : Prop :=
  a.room = expectedRoom ∧
  a.biscuitRootPk = expectedRootPk ∧
  a.slhPk = expectedSlhPk ∧
  a.validHashSig = true

/-- Authorization gated by a verified hash-based `BiscuitRootAnchor`. -/
def anchoredAuthorizes (expectedRoom : String) (expectedRootPk expectedSlhPk : Nat)
    (a : RootAnchor) (g : Grant) (now : Nat) (r : Req) : Prop :=
  validAnchor expectedRoom expectedRootPk expectedSlhPk a ∧ authorizes g a.revoked now r

theorem anchor_cross_room_denies {expectedRoom : String} {expectedRootPk expectedSlhPk : Nat}
    {a : RootAnchor} {g : Grant} {now : Nat} {r : Req}
    (h : a.room ≠ expectedRoom) :
    ¬ anchoredAuthorizes expectedRoom expectedRootPk expectedSlhPk a g now r := by
  intro ⟨⟨hroom, _⟩, _⟩
  exact h hroom

theorem anchor_unpinned_slh_denies {expectedRoom : String} {expectedRootPk expectedSlhPk : Nat}
    {a : RootAnchor} {g : Grant} {now : Nat} {r : Req}
    (h : a.slhPk ≠ expectedSlhPk) :
    ¬ anchoredAuthorizes expectedRoom expectedRootPk expectedSlhPk a g now r := by
  intro ⟨⟨_, _, hslh, _⟩, _⟩
  exact h hslh

theorem anchor_invalid_sig_denies {expectedRoom : String} {expectedRootPk expectedSlhPk : Nat}
    {a : RootAnchor} {g : Grant} {now : Nat} {r : Req}
    (h : a.validHashSig = false) :
    ¬ anchoredAuthorizes expectedRoom expectedRootPk expectedSlhPk a g now r := by
  intro ⟨⟨_, _, _, hsig⟩, _⟩
  rw [h] at hsig
  contradiction

theorem anchor_revoked_denies {expectedRoom : String} {expectedRootPk expectedSlhPk : Nat}
    {a : RootAnchor} {g : Grant} {now : Nat} {r : Req}
    (h : g.id ∈ a.revoked) :
    ¬ anchoredAuthorizes expectedRoom expectedRootPk expectedSlhPk a g now r := by
  intro ⟨_, hauth⟩
  exact revoked_denies h hauth

/-- Monotonicity across anchored epochs: adding revocations never widens authority. -/
theorem anchor_revocation_monotonic {expectedRoom : String} {expectedRootPk expectedSlhPk : Nat}
    {a₁ a₂ : RootAnchor} {g : Grant} {now : Nat} {r : Req}
    (hv₁ : validAnchor expectedRoom expectedRootPk expectedSlhPk a₁)
    (hsub : ∀ id, id ∈ a₁.revoked → id ∈ a₂.revoked)
    (ha₂ : anchoredAuthorizes expectedRoom expectedRootPk expectedSlhPk a₂ g now r) :
    anchoredAuthorizes expectedRoom expectedRootPk expectedSlhPk a₁ g now r :=
  ⟨hv₁, authorizes_mono_revoked hsub ha₂.2⟩

/-- Host cosignature coverage on a `RoomLog` checkpoint (`crates/playbox-core/src/roomlog.rs`). -/
structure TriSig where
  host       : Nat
  hasEd25519 : Bool
  hasMlDsa   : Bool
  hasSlhDsa  : Bool

def isHybridSigned (s : TriSig) : Bool :=
  s.hasEd25519 && s.hasMlDsa

def isPqaiSigned (s : TriSig) : Bool :=
  s.hasEd25519 && s.hasMlDsa && s.hasSlhDsa

/-- Classical-only or lattice-only cosignatures can never satisfy `isPqaiSigned`. -/
theorem pqai_requires_hash_sig (s : TriSig) (h : isPqaiSigned s = true) :
    s.hasSlhDsa = true := by
  cases he : s.hasEd25519 <;> cases hm : s.hasMlDsa <;> cases hs : s.hasSlhDsa <;>
    simp [isPqaiSigned, he, hm, hs] at h ⊢

theorem pqai_implies_hybrid (s : TriSig) (h : isPqaiSigned s = true) :
    isHybridSigned s = true := by
  cases he : s.hasEd25519 <;> cases hm : s.hasMlDsa <;> cases hs : s.hasSlhDsa <;>
    simp [isPqaiSigned, isHybridSigned, he, hm, hs] at h ⊢

/-- PQAI tri-signed count never exceeds hybrid (Ed25519+ML-DSA) count. -/
theorem pqai_count_le_hybrid_count (sigs : List TriSig) :
    (sigs.filter isPqaiSigned).length ≤ (sigs.filter isHybridSigned).length := by
  induction sigs with
  | nil => simp
  | cons s rest ih =>
    by_cases hp : isPqaiSigned s = true
    · have hh : isHybridSigned s = true := pqai_implies_hybrid s hp
      simp [hp, hh]
      omega
    · have hp_false : isPqaiSigned s = false := by
        cases h : isPqaiSigned s
        · rfl
        · exact absurd h hp
      cases hh : isHybridSigned s <;> simp [hp_false, hh] <;> omega

/-- Disjoint filtered subsets of a host list cannot exceed the host list's length. -/
theorem filter_disjoint_add_le {α : Type} (xs : List α) (p q : α → Bool)
    (hdisj : ∀ x ∈ xs, ¬ (p x = true ∧ q x = true)) :
    (xs.filter p).length + (xs.filter q).length ≤ xs.length := by
  induction xs with
  | nil => simp
  | cons x rest ih =>
    have hrest : ∀ y ∈ rest, ¬ (p y = true ∧ q y = true) :=
      fun y hy => hdisj y (by simp [hy])
    have hih := ih hrest
    have hx := hdisj x (by simp)
    cases hp : p x <;> cases hq : q x
    · simp [hp, hq]; omega
    · simp [hp, hq]; omega
    · simp [hp, hq]; omega
    · exact absurd ⟨hp, hq⟩ hx

/-- Strict majority (`2 * k > n`) PQAI checkpoint safety: if every host in `hosts`
tri-signs at most one checkpoint digest at a given `(epoch, index)`, then two
conflicting digests cannot both reach a PQAI quorum of size `k`. -/
theorem pqai_checkpoint_safety {hosts : List Nat} {k : Nat}
    {signed₁ signed₂ : Nat → Bool} {digest₁ digest₂ : Nat}
    (hmaj : 2 * k > hosts.length)
    (hq₁ : (hosts.filter signed₁).length ≥ k)
    (hq₂ : (hosts.filter signed₂).length ≥ k)
    (hsingle : ∀ h ∈ hosts, signed₁ h = true → signed₂ h = true → digest₁ = digest₂) :
    digest₁ = digest₂ := by
  by_cases heq : digest₁ = digest₂
  · exact heq
  · exfalso
    have hdisj : ∀ h ∈ hosts, ¬ (signed₁ h = true ∧ signed₂ h = true) := by
      intro h hh ⟨h1, h2⟩
      exact heq (hsingle h hh h1 h2)
    have hle := filter_disjoint_add_le hosts signed₁ signed₂ hdisj
    omega

end Playbox.Pqai
