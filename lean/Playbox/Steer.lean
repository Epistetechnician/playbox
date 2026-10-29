/-!
# Steering grants: attenuation never widens authority

Models the steering limits of a grant (`SteerCaps` in `crates/playbox-net/src/grants.rs`): the models it
names, a layer range, a strength cap, the modes it allows, and a token cap per run. As in `Grants.lean`,
crypto is trusted and this file models only the logic.

In the implementation a member narrows a grant by appending a Biscuit block. A block can only add checks,
and a request must pass every check, so the narrowed grant authorizes a request only if the parent does
too. `Policy.restrict` is that fact; `SteerGrant.le` is the same property stated on the limits themselves,
and `narrow` is what `steer_narrowing` computes.
-/

namespace Playbox

structure SteerGrant where
  models    : List Nat   -- ids of weights the member may steer
  layerMin  : Nat
  layerMax  : Nat
  maxCoef   : Nat        -- largest |strength|, in thousandths of a norm
  modes     : List Nat   -- 0 add, 1 clamp, 2 ablate
  maxTokens : Nat

structure SteerReq where
  model   : Nat
  layerLo : Nat
  layerHi : Nat
  coef    : Nat          -- absolute value of the requested strength
  mode    : Nat
  tokens  : Nat

/-- `c` only narrows `p`. -/
def SteerGrant.le (c p : SteerGrant) : Prop :=
  (∀ m, m ∈ c.models → m ∈ p.models) ∧ p.layerMin ≤ c.layerMin ∧ c.layerMax ≤ p.layerMax ∧
  c.maxCoef ≤ p.maxCoef ∧ (∀ m, m ∈ c.modes → m ∈ p.modes) ∧ c.maxTokens ≤ p.maxTokens

/-- The request fits inside the steering limits. -/
def steerAllows (g : SteerGrant) (r : SteerReq) : Prop :=
  r.model ∈ g.models ∧ g.layerMin ≤ r.layerLo ∧ r.layerHi ≤ g.layerMax ∧
  r.coef ≤ g.maxCoef ∧ r.mode ∈ g.modes ∧ r.tokens ≤ g.maxTokens

theorem SteerGrant.le_refl (g : SteerGrant) : SteerGrant.le g g :=
  ⟨fun _ h => h, Nat.le_refl _, Nat.le_refl _, Nat.le_refl _, fun _ h => h, Nat.le_refl _⟩

theorem SteerGrant.le_trans {a b c : SteerGrant} (h1 : SteerGrant.le a b) (h2 : SteerGrant.le b c) :
    SteerGrant.le a c := by
  obtain ⟨a1, a2, a3, a4, a5, a6⟩ := h1
  obtain ⟨b1, b2, b3, b4, b5, b6⟩ := h2
  exact ⟨fun m hm => b1 m (a1 m hm), Nat.le_trans b2 a2, Nat.le_trans a3 b3, Nat.le_trans a4 b4,
    fun m hm => b5 m (a5 m hm), Nat.le_trans a6 b6⟩

/-- Anything a narrowed steering grant allows, its parent also allows. -/
theorem steerAllows_mono {c p : SteerGrant} {r : SteerReq} (h : SteerGrant.le c p)
    (ha : steerAllows c r) : steerAllows p r := by
  obtain ⟨h1, h2, h3, h4, h5, h6⟩ := h
  obtain ⟨a1, a2, a3, a4, a5, a6⟩ := ha
  exact ⟨h1 _ a1, Nat.le_trans h2 a2, Nat.le_trans a3 h3, Nat.le_trans a4 h4, h5 _ a5,
    Nat.le_trans a6 h6⟩

/-- What `steer_narrowing` does: cap the top layer, the strength and the tokens, and keep only the listed
modes. Each cap can only lower the parent's, and a listed mode the parent lacks stays refused. -/
def narrow (g : SteerGrant) (layerMax : Nat) (coef : Nat) (modes : List Nat) (tokens : Nat) : SteerGrant :=
  { g with
    layerMax := Nat.min g.layerMax layerMax
    maxCoef := Nat.min g.maxCoef coef
    modes := g.modes.filter (fun m => decide (m ∈ modes))
    maxTokens := Nat.min g.maxTokens tokens }

theorem narrow_le (g : SteerGrant) (layerMax coef : Nat) (modes : List Nat) (tokens : Nat) :
    SteerGrant.le (narrow g layerMax coef modes tokens) g := by
  refine ⟨fun m hm => hm, Nat.le_refl _, Nat.min_le_left _ _, Nat.min_le_left _ _, ?_,
    Nat.min_le_left _ _⟩
  intro m hm
  exact (List.mem_filter.mp hm).1

/-- A narrowed grant allows a request only if the parent does too, however the limits were chosen. -/
theorem narrow_cannot_widen (g : SteerGrant) (layerMax coef : Nat) (modes : List Nat) (tokens : Nat)
    {r : SteerReq} (h : steerAllows (narrow g layerMax coef modes tokens) r) : steerAllows g r :=
  steerAllows_mono (narrow_le g layerMax coef modes tokens) h

/-- The narrowing also never allows a mode outside the list it was given. -/
theorem narrow_respects_modes (g : SteerGrant) (layerMax coef : Nat) (modes : List Nat) (tokens : Nat)
    {r : SteerReq} (h : steerAllows (narrow g layerMax coef modes tokens) r) : r.mode ∈ modes := by
  have := (List.mem_filter.mp h.2.2.2.2.1).2
  simpa using this

/-- Narrowing twice is still a narrowing of the original: no sequence of blocks widens a grant. -/
theorem narrow_twice_le (g : SteerGrant) (l1 c1 : Nat) (m1 : List Nat) (t1 : Nat)
    (l2 c2 : Nat) (m2 : List Nat) (t2 : Nat) :
    SteerGrant.le (narrow (narrow g l1 c1 m1 t1) l2 c2 m2 t2) g :=
  SteerGrant.le_trans (narrow_le _ l2 c2 m2 t2) (narrow_le g l1 c1 m1 t1)

/-- A chain of delegations, each narrowing the last. -/
def steerChain : List SteerGrant → Prop
  | [] => True
  | [_] => True
  | a :: b :: rest => SteerGrant.le b a ∧ steerChain (b :: rest)

/-- Along a chain, no later grant exceeds the root's steering limits. -/
theorem steerChain_le_root : ∀ (rest : List SteerGrant) (a : SteerGrant),
    steerChain (a :: rest) → ∀ g, g ∈ rest → SteerGrant.le g a
  | [], _, _, g, hg => by cases hg
  | b :: rest, a, ⟨hba, hrest⟩, g, hg => by
    cases hg with
    | head => exact hba
    | tail _ hmem => exact SteerGrant.le_trans (steerChain_le_root rest b hrest g hmem) hba

end Playbox
