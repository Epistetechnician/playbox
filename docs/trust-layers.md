# Trust layers

Five pieces that sit on top of grants, histories and attestation. Each states what it proves and what
it does not. All are in `playbox-core` and tested; the broker and memory are wired into the MCP server.

| Piece | Module | Wired into |
|---|---|---|
| 1. Admission broker (text from other members cannot drive actions unreviewed) | `taint` | `playbox_delegate`, `playbox approve` |
| 2. Confidential Space verifier (`tdx`, T3) | `confidential` | `PlatformVerifier` registry, `PLAYBOX_ATTEST=tdx` |
| 3. Signed evaluator, sealed holdout, variant lineage | `evaluator` | library only |
| 4. Shared memory with provenance | `memory` | `playbox_remember`, `playbox_recall` |
| 5. Model receipts and witnessed checkpoints | `receipt`, `witness` | library only |

Shared plumbing: `record` is a small signed-text format (domain line, fields, signer, signature) used by
all of them. `history::Entry::check_signature` was added so a witness can check entries without replaying
a whole log.

## 1. Admission broker

Grants limit what a guest can *do*. They do not limit what a guest's output *says* to the host's own
agent, which reads it and may act on it. The broker closes that gap in the shape of information-flow
control plus a capability broker:

- Text from outside the host's trust boundary is registered with a source and a trust level
  (`Untrusted`, `Reviewed`, `Trusted`).
- Before an action runs, the broker checks whether its arguments carry that text. If the source is less
  trusted than the action's rule allows, the action is not run: the agent gets a ticket.
- A pinned approver (a person, using `playbox approve`) signs a capability bound to this action and exactly
  these arguments. It is single-use and expires. Different arguments, replays, expiry and unpinned or
  forged signers all fail. Three failed redemptions freeze every non-read action until an approver thaws.
- Every decision is appended to a signed journal.

In the MCP server, set `PLAYBOX_APPROVERS` to the approver's public key (`playbox approver-key` prints
one). Without it the broker only reports an `admission` note in `playbox_delegate` results. Task stdout,
stderr and patches are registered as untrusted automatically.

**Does not prove:** detection is lexical (three-word windows and long identifier-like tokens). A model that
paraphrases an injected instruction passes. The agent also holds the capability string, so it must not hold
the approver key. This is a speed bump with an audit trail, not a proof of non-influence. `playbox_delegate`, `playbox_push` and `playbox_revoke` are guarded; each takes a `capability`.

## 2. Confidential Space verifier (T3)

`ConfidentialSpaceVerifier` checks a Google Cloud Attestation token offline against pinned signing keys:
RS256 only, issuer, audience, expiry, that `eat_nonce` carries this statement's binding, Intel TDX
hardware, `CONFIDENTIAL_SPACE` image, debug disabled since boot, secure boot, a STABLE release, optional
project allow-list, and the container image digest. With the digest on the allow-list it reports T3;
with none pinned it reports T1 (a genuine confidential VM running an unknown workload); anything else is
rejected.

Configure with `PLAYBOX_CS_JWKS` (file), `PLAYBOX_CS_AUDIENCE`, `PLAYBOX_CS_IMAGES`, `PLAYBOX_CS_PROJECTS`.
The host produces evidence with `PLAYBOX_ATTEST=tdx` (it asks the launcher socket for a token whose nonce
is the statement binding). Use a Playbox-specific audience so a leaked token cannot be federated to cloud
resources.

**Tested against a real token:** on 2026-10-29 a `c3-standard-4` TDX Confidential Space VM produced a token that the verifier checks (fixture in `tests/fixtures/confidential-space`, with Google's keys). That found a bug: Google reports the hardware model as `GCP_INTEL_TDX`, which the verifier had rejected. **End to end:** on 2026-10-29 a Playbox host (`playbox up`, `PLAYBOX_ATTEST=tdx`) ran as the workload of a Confidential Space TDX VM; a guest on a Mac, across the internet via n0's relay, joined with `min_host_tier: T3` and verified the host at T3 (image digest on its allow-list). That found a second bug: the launcher answers with HTTP chunked encoding, which the provider did not decode (now fixed, with a unit test). **Not tested:** the verifier rejecting a real token with the wrong digest or project (only the fixture covers this), and long-running hosts (the token expires; the host asks for one per statement).

## 3. Verifiable self-improvement

- `Evaluator`: a manifest signed by a custodian (not the agent): harness hash, holdout root, command,
  protected paths, minimum runs. Changing it needs a new custodian-signed manifest naming the one it
  supersedes. This keeps the evaluator fixed from inside the loop, the opposite of co-evolving it.
- `Holdout`: Merkle root over salted items, with per-item proofs that do not leak other items.
- `Variant` / `Lineage`: a signed claim "this patch on that parent scored these per-run numbers under that
  evaluator, at this cost". Rejected if the patch touches a protected path, the host-computed patch is not
  the signed one, the evaluator changed without a rotation, the parent is unknown, or there are too few
  runs. `improvement` returns a one-sided lower confidence bound, so noise is not an improvement.

**Does not prove:** the scores were honestly produced unless the custodian (or an attested runner) ran the
evaluation. It cannot see reward hacking inside files the variant is allowed to change. A fixed holdout is
eventually overfit by many queries; `total_cost` records how many were spent.

## 4. Shared memory

Signed, chained entries; guest entries are quarantined until a pinned reviewer accepts them; facts and
decisions need evidence; trust follows `derived_from` transitively, so revoking an author or rejecting an
entry also removes everything built on it; conflicts on a topic are listed for a person, not merged;
recall registers text with the taint tracker so memory text pasted into a command is caught by the
broker. State is rebuilt by replaying the event log.

**Does not prove:** that a fact is true. A reviewer who accepts a lie has accepted it.

## 5. Model receipts and witnessed checkpoints

- `Receipt`: someone other than the session key signs `(provider, model, request hash, response hash)`.
  Levels: provider-signed, notarized, self-claimed, none. `coverage` matches a session's `model_response`
  entries to receipts (and checks the receipt's request hash against the preceding `prompt`); `require`
  turns that into a room policy. No provider signs responses today, so the practical level is a notary or a
  TEE proxy you pin.
- `Checkpoint` / `Witness`: the log key signs `(size, head)`. A witness cosigns only after replaying the
  entries that extend its last view; it refuses rollbacks, and two different heads for one size produce a
  verifiable `Equivocation` proof. `verify_quorum` checks m-of-n distinct pinned witnesses.

**Does not prove:** that a prompt was the one the agent really saw, or that the notary/provider key is
honest. Witnessing is linear in log growth (no Merkle log yet) and means nothing if witnesses are not
independent of the host.

## Not built

Network protocol for collecting cosignatures; receipt collection from real notaries (TLSNotary) or TEE
proxies; a network protocol that carries memory events between members (events are exchanged by hand through `playbox_memory_export` / `playbox_memory_ingest`); a real Confidential
Space run.

Memory in the MCP server: entries are signed by the sidecar's session key (the owner), persisted in `memory.json` under the home directory and replayed on start. Review of other authors' entries and `playbox_use_host`-style sharing across members are not exposed yet.

Other MCP tools: `playbox_memory_ingest` / `playbox_memory_export` (quarantined entries leave quarantine only on an event signed by a key in `PLAYBOX_APPROVERS`, produced offline with `playbox memory-review <id> --accept|--reject`), `playbox_receipts_check`, `playbox_checkpoint` / `playbox_checkpoint_verify`, `playbox_lineage_check`. These verify what you give them; nothing collects receipts or cosignatures for you.
