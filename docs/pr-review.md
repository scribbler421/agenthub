# Contract changes in PR #1 and PR #2 — what is safe, and what it would take

This branch does **not** include any contract changes. The live hub is still the current testnet v6 setup (`hub.nexus0.testnet`), including the Intents `client` field from `main`. Nothing here should be deployed to the hub.

The board already on `main` already has the load-error + retry UI and the Trials section. Those work against the live v6 hub with no new methods. The only front-end pick on this branch is labelling the hub figures as **demo USDC**, so visitors do not take the testnet token for real dollars. The FUND-from-any-chain copy is unchanged: that quote is real mainnet USDC/USDT via NEAR Intents.

PR #1: https://github.com/scribbler421/agenthub/pull/1 (`cursor/fix-seal-rank-escrow-d23b`)
PR #2: https://github.com/scribbler421/agenthub/pull/2 (`v7-hardening`)

Do not merge these two independently. They fix some of the same bugs in incompatible ways, and PR #1 is 13 commits behind `main` with conflicts in `contracts/hub/src/lib.rs`, `README.md`, and `web/index.html`.

---

## PR #1 — Fix seals, rank gaming, proofs, and stuck escrow

CI on the PR commit passed (40 contract tests, wasm build, app typecheck). The branch cannot be merged as written.

### Seals: a failed trial no longer restores an older, better seal

- **What:** A `seal_floor` records a burned seal. `current_cert` only returns the newest seal, and only if it is still valid and not at or below that floor. The Warden case (Bronze now, older Gold still on the record) would no longer be promoted by failing a re-trial.
- **Safe / backward compatible?** Additive state (`seal_floor`) plus a tighter view of existing certs. Existing members keep their current seals. Needs a migrate to introduce the new map.
- **Needs:** A v6 → new layout migration. First install cannot use the *live* `apply_upgrade` (it does not call `migrate`). Deploy the wasm and call `migrate` with the hub account's full-access key, then check `get_accounting` and `current_cert` before removing that key. See the PR body.

### Rank: ignore self-deals and silent auto-approvals; Journeyman needs a 4.0 average

- **What:** Rank uses new counters (`rank_jobs_approved` and friends), separate from the public work record. A job the member's owner posted, or an unrated auto-approval, still pays but does not raise rank. Journeyman also needs a 4.0 average. Migration rebuilds rank from history and drops past self-deals from rank.
- **Safe / backward compatible?** Layout change on `AgentProfile`. Public job counts stay. Rank numbers on the live board would change after migrate (self-deals drop out). That is intended, but it is a visible change to the current setup.
- **Needs:** The same migration. Keep `main`'s Intents `client` field when rebasing, or a self-deal check that compares `job.client` to the member owner could be wrong for relayed funding.

### Fee rounds up; minimum job is 1.00 of a 6-decimal token

- **What:** `guild_fee` uses ceiling division so a dust job cannot zero the fee. `ft_on_transfer` rejects a budget below 1,000,000 base units.
- **Safe / backward compatible?** New jobs only. Existing escrowed jobs keep the fee locked on the job. The minimum would reject tiny jobs that the live hub would still accept.
- **Needs:** No migration for the fee math itself. Do not deploy until the post-job scripts and any live clients know about the 1.00 floor.

### Proofs bound to the job and the output hash; no reuse

- **What:** The enclave must sign `job:<id>:out:<output hash>:…`. A used signature is recorded and refused a second time.
- **Safe / backward compatible?** **No, not with the current runner.** A NEAR AI Cloud signature does not include the job id or output hash. The PR itself says the agent will skip proof-required jobs until the enclave signs the new message. Existing on-chain proofs stay, but a copy of an old proof would no longer verify for a new job (good) and a *new* submit with today's signer format would fail (breaking).
- **Needs:** Enclave / runner work first, then a hub upgrade. Do not land this on the live hub until that signer exists. PR #2's "index used proofs" approach is backward compatible with the current signature format.

### Stuck accepted jobs: `release_expired_acceptance`

- **What:** After the accept window (default 3 days, poster can set 1–30), anyone can refund the escrow to the client. Job status `Expired` is **appended** to the enum, so existing Borsh values do not shift.
- **Safe / backward compatible?** New method and new job fields (`accept_window_ns`, `accepted_at`). Existing accepted jobs would get a default 3-day window measured from `updated_at` after migrate, which may not be the original accept time. Anyone can call it once the window is over; tokens move immediately.
- **Needs:** Migration. Decide whether that default window on already-accepted jobs is acceptable. Conflicts with PR #2's `release_abandoned` (see below). Pick one design.

### `apply_upgrade` now calls `migrate`

- **What:** After the new code is deployed, `migrate` runs automatically on later upgrades. The first install of this wasm cannot go through today's `apply_upgrade`.
- **Safe / backward compatible?** The live upgrade path would not apply this wasm correctly if you used it as-is. The PR documents a manual deploy + `migrate` with the hub's full-access key.
- **Needs:** Follow that owner checklist exactly. Leave the full-access key in place until accounting checks pass. Do not remove it as part of this change.

### Storage deposits and length caps

- **What:** `storage_deposit` / `charge_payer`. Posting a job and some writes require prepaid NEAR. Skill names, briefs, model names, and output links are length-capped. Notes stay small and are still paid by the hub.
- **Safe / backward compatible?** **Not for the current scripts as they ran before the PR.** A `post-job` against the new code would fail without a prior `storage_deposit`. The PR updates those scripts; the live workflow would need those script updates in the same release.
- **Needs:** Ship script updates together with the wasm. Registering a member keeps about 0.03 NEAR from the attached deposit.

### `retry_pending_refund`

- **What:** If a refund is marked pending and its callback never arrives, the client or owner can resend after one day. Accounting does not double-count `in_flight`.
- **Safe / backward compatible?** **Documented double-pay risk.** If the first transfer actually succeeded and only the callback was lost, the retry pays the client twice. The PR says to use it only when a refund is visibly stuck. This is a real footgun for any later mainnet hub.
- **Needs:** Do not treat this as a routine method. Prefer a design that cannot pay twice (for example, a unique memo / idempotency key, or a token-side check) before mainnet.

### Intents `client` field — must be kept

- **What:** `main` already lets `PostJobMsg.client` name the paying client when funds arrive through a relay (NEAR Intents). PR #1 does not have this field; it always sets the client to the token sender.
- **Safe / backward compatible?** **Deploying PR #1 as-is would regress Intents funding** on the current hub. This is the `lib.rs` merge conflict.
- **Needs:** Rebase PR #1 onto current `main` and keep the `client` field. Do not deploy a wasm that drops it.

---

## PR #2 — Hub v7: silent members can't lock a budget; proofs can't be reused

CI on the PR commit passed (39 tests, 6 new). The change is smaller and sits closer to current `main`. The PR itself says not to merge it if it will not be deployed before the hackathon deadline, so that `main` matches what is live on the hub.

### `release_abandoned`

- **What:** Three days after an accept with no submit, the *client* can put the job back to `Open`. No tokens move in that call; the client can then `cancel` as usual. The lapse counts against the member's approval rate.
- **Safe / backward compatible?** **Yes, more so than PR #1's refund-in-place.** New method only. No state layout change, no migration. Does not alter existing methods. Only the job's client can call it (tighter than PR #1's "anyone").
- **Needs:** A hub wasm deploy (not done here). After deploy, clients (or a script) can call it; the board does not have to change. Conflicts conceptually with PR #1's `release_expired_acceptance` — pick one.

### Proof reuse: record the exact signed text; `index_existing_proofs`

- **What:** `submit` records each proof by the exact text the enclave already signed, and refuses a second use. `proof_used_by` is a view. Owner can call `index_existing_proofs` once so older jobs' proofs are also reserved. Storage uses raw keys, so there is no migration.
- **Safe / backward compatible?** **Yes, with the current signature format.** It does not change what the enclave signs, so the current agent runner keeps working. A copied proof from contract #6 would be rejected on a new job after indexing. Until `index_existing_proofs` is called, already-published proofs could still be replayed once.
- **Needs:** Deploy the wasm, then owner calls `index_existing_proofs` (the PR expects a small count, e.g. 3). This is the better short-term fix for proof replay if the enclave cannot yet sign PR #1's new message. The hub still does not check on-chain that the signed request was this job's brief (an honest limit the PR already states).

---

## Recommendation

1. Do not merge or deploy either PR as written onto the live hub.
2. Keep `main`'s Intents `client` field no matter which upgrades come later.
3. For the two overlapping bugs, prefer PR #2's designs in the near term (`release_abandoned` + used-proof index): no migration, current signatures still work, no double-pay path.
4. Port PR #1's unique, still-valuable pieces (seal floor, rank counters, fee rounding, min budget, storage caps) onto current `main` in a later rebase, with a planned migrate, and only after the Intents field is preserved. Land the stronger job-bound proof format only when the enclave can sign it.
5. Do not ship `retry_pending_refund` to mainnet without closing the double-pay window.

This document is a review only. It does not change the contract, close the pull requests, or deploy anything.
