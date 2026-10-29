/-!
# Playbox grants: abstract model

Crypto (signatures, token encoding) is trusted. This file models only the *logic*:
attenuation never widens authority, and expired or revoked grants authorize nothing.
-/

namespace Playbox

structure Grant where
  id      : Nat
  cpu     : Nat
  mem     : Nat
  timeout : Nat
  expiry  : Nat
  net     : Nat          -- 0 = none, 1 = internet, 2 = internet+lan
  cmds    : List String

structure Req where
  cmd     : String
  cpu     : Nat
  mem     : Nat
  timeout : Nat
  net     : Nat

/-- `c` is a valid attenuation of `p`: it only narrows. -/
def Grant.le (c p : Grant) : Prop :=
  c.cpu ≤ p.cpu ∧ c.mem ≤ p.mem ∧ c.timeout ≤ p.timeout ∧ c.expiry ≤ p.expiry ∧
  c.net ≤ p.net ∧ (∀ x, x ∈ c.cmds → x ∈ p.cmds)

/-- The request fits inside the grant's limits at time `now`. -/
def withinLimits (g : Grant) (now : Nat) (r : Req) : Prop :=
  now < g.expiry ∧ r.cmd ∈ g.cmds ∧ r.cpu ≤ g.cpu ∧ r.mem ≤ g.mem ∧
  r.timeout ≤ g.timeout ∧ r.net ≤ g.net

/-- Full authorization: within limits and not revoked. -/
def authorizes (g : Grant) (revoked : List Nat) (now : Nat) (r : Req) : Prop :=
  withinLimits g now r ∧ g.id ∉ revoked

theorem Grant.le_refl (g : Grant) : Grant.le g g :=
  ⟨Nat.le_refl _, Nat.le_refl _, Nat.le_refl _, Nat.le_refl _, Nat.le_refl _, fun _ h => h⟩

theorem Grant.le_trans {a b c : Grant} (h1 : Grant.le a b) (h2 : Grant.le b c) : Grant.le a c := by
  obtain ⟨a1, a2, a3, a4, a5, a6⟩ := h1
  obtain ⟨b1, b2, b3, b4, b5, b6⟩ := h2
  exact ⟨Nat.le_trans a1 b1, Nat.le_trans a2 b2, Nat.le_trans a3 b3, Nat.le_trans a4 b4,
    Nat.le_trans a5 b5, fun x hx => b6 x (a6 x hx)⟩

/-- Property 1: anything an attenuated grant allows, its parent also allows. -/
theorem withinLimits_mono {c p : Grant} {now : Nat} {r : Req}
    (h : Grant.le c p) (hw : withinLimits c now r) : withinLimits p now r := by
  obtain ⟨hcpu, hmem, hto, hexp, hnet, hcmd⟩ := h
  obtain ⟨w1, w2, w3, w4, w5, w6⟩ := hw
  exact ⟨Nat.lt_of_lt_of_le w1 hexp, hcmd _ w2, Nat.le_trans w3 hcpu, Nat.le_trans w4 hmem,
    Nat.le_trans w5 hto, Nat.le_trans w6 hnet⟩

/-- Property 2a: an expired grant authorizes nothing. -/
theorem expired_denies {g : Grant} {revoked : List Nat} {now : Nat} {r : Req}
    (h : g.expiry ≤ now) : ¬ authorizes g revoked now r := by
  intro ⟨⟨hlt, _⟩, _⟩
  exact Nat.lt_irrefl _ (Nat.lt_of_lt_of_le hlt h)

/-- Property 2b: a revoked grant authorizes nothing. -/
theorem revoked_denies {g : Grant} {revoked : List Nat} {now : Nat} {r : Req}
    (h : g.id ∈ revoked) : ¬ authorizes g revoked now r := by
  intro ⟨_, hnot⟩
  exact hnot h

/-- A delegation chain where every link attenuates its predecessor. -/
def attenuates : List Grant → Prop
  | [] => True
  | [_] => True
  | a :: b :: rest => Grant.le b a ∧ attenuates (b :: rest)

/-- Property 3: along a chain, no later grant exceeds the root's limits. -/
theorem chain_le_root : ∀ (rest : List Grant) (a : Grant),
    attenuates (a :: rest) → ∀ g, g ∈ rest → Grant.le g a
  | [], _, _, g, hg => by cases hg
  | b :: rest, a, ⟨hba, hrest⟩, g, hg => by
    cases hg with
    | head => exact hba
    | tail _ hmem => exact Grant.le_trans (chain_le_root rest b hrest g hmem) hba

end Playbox

namespace Playbox

/-- Property 4 (replication): learning of more revocations never authorizes anything new. If every id
revoked on one host is also revoked on another, whatever the second authorizes the first does too.
So a host that has heard fewer notices is only ever *more* permissive, never less, and receiving a
notice (in any order, any number of times) can only shrink what is authorized. -/
theorem authorizes_mono_revoked {g : Grant} {revoked revoked' : List Nat} {now : Nat} {r : Req}
    (h : ∀ x, x ∈ revoked → x ∈ revoked') (ha : authorizes g revoked' now r) :
    authorizes g revoked now r :=
  ⟨ha.1, fun hmem => ha.2 (h _ hmem)⟩

end Playbox
