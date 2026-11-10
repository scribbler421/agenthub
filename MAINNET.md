# Taking the hub to mainnet

Nothing here has been done yet. The hub runs on testnet (`hub.nexus0.testnet`, v6) with a
single operator account holding every key. This is the handover plan, in order, with the
reasons. None of it changes keys or moves funds until someone follows it on purpose.

## What the contract already enforces (v6)

| Concern | Mechanism |
| --- | --- |
| A silent client cannot hold pay hostage | `review_window` per job (default 5 days); after it, anyone can `claim_auto_approve` |
| A reject cannot take the money back by itself | `reject` opens a dispute; the **arbiter** splits it with `resolve_dispute(job_id, member_bps)`; after 30 days anyone can `claim_dispute_fallback` (50/50) |
| A silent member cannot lock a budget (v7) | a job stays `Accepted` for at most `ACCEPT_WINDOW_NS` (3 days) without a submit; after that the client can `release_abandoned` it back to `Open` and `cancel` for a refund; the lapse counts against the member's approval rate |
| An execution proof cannot be reused (v7) | `submit` records each proof by the text the enclave signed and refuses a second use; `index_existing_proofs` (owner, once after the upgrade) records the proofs older jobs already carry |
| A refund cannot be lost or paid twice | job is marked `Pending` *before* the transfer; `on_refund` (private) marks `Refunded` or `Failed`; `retry_refund` re-sends a failed one; every settlement path checks `refund == None` |
| The fee cannot be changed under a live job | `fee_bps` is copied onto the job when it is funded and used at settlement; hub fee hard-capped at `MAX_FEE_BPS = 800` (8%); `fee_changed` event |
| The books can be checked by anyone | `get_accounting()` returns escrowed + member balances + fees + in-flight + refunds owed; `total` must equal the hub's balance on the token contract |
| Ownership moves to a multisig without a typo orphaning the hub | `propose_owner` / `accept_owner` (the new owner must call in) |
| Upgrades are visible before they happen | `propose_upgrade(code)` stages the wasm; `apply_upgrade` only after `upgrade_delay` (60 s on testnet; 48 h planned); `set_upgrade_delay` can only lengthen it |
| Incidents can be contained without trapping funds | `set_paused` blocks funding, accepts, submits and withdrawals; approvals, disputes and refunds keep working |

Unit tests assert the accounting identity after every operation
(`contracts/hub/src/lib.rs`, `check(...)`); `npm run attack:escrow` repeats the attacks on
the live testnet contract and checks that no balance moved.

## Handover, step by step

1. **Deploy v6 to a fresh mainnet account** (`hub.<org>.near`) with `new(owner, token, 600, 48h)`
   where `token` is Circle USDC on NEAR (`17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1`).
   Fund it with enough NEAR for storage (budget ~2 NEAR per 1,000 jobs) and register it on
   the token with `storage_deposit`.
2. **Audit or bug bounty first.** Real USDC sits in this contract. The money paths are small
   (`ft_on_transfer`, `settle`, `start_refund`, `on_refund`, `withdraw`, `withdraw_fees`) and
   are the only things an auditor has to read closely.
3. **Create the multisig** (a NEAR multisig or a Sputnik DAO) with at least 2-of-3 signers, and
   register it on the token so fee withdrawals to it can land.
4. **Hand over ownership:** operator calls `propose_owner(multisig)`; the multisig calls
   `accept_owner()`. The arbiter follows the owner automatically if they were the same account;
   set a separate `set_arbiter` if disputes should be settled by someone else (a DAO vote, a
   neutral third party).
5. **Set the upgrade delay** to 48 hours: `set_upgrade_delay(172_800_000_000_000)`. It can be
   lengthened later but never shortened without an upgrade, which itself waits 48 hours.
6. **Remove the full-access key from the hub account.** This is the step that makes the
   upgrade delay real: until it is done, whoever holds that key can `deploy_contract`
   instantly, and nothing in the contract can stop them. After it, the only way to change the
   code is `propose_upgrade` → wait → `apply_upgrade`, both owner-only, i.e. multisig-only.
   Do it from the operator: `near account delete-key hub.<org>.near <full-access-public-key>`.
   Check with `near account list-keys hub.<org>.near` that only function-call keys (if any) remain.
7. **Register the attested enclave signers** (`add_trusted_signer`) from a fresh NEAR AI
   attestation, and mint no seals until the first mainnet trial round is published.
8. **Turn on monitoring:** a job that polls `get_accounting().total` against
   `ft_balance_of(hub)` on the token every few minutes and pages on any drift, plus the
   `refunds_owed` counter (should stay zero; anything else means a client needs
   `retry_refund` or is unregistered on the token).

## What stays off-chain, and known limits

- An execution proof shows that an attested enclave produced *a* response to *a* request,
  and the hub refuses to accept the same proof twice. The hub does not check on-chain that
  the request was this job's brief: that needs the request body (or a commitment to the job
  id inside it) to be checked against the signed `request_hash`, which is the next step.
  Until then a `require_proof` job proves "enclave-run, not reused", not "enclave-run on
  exactly this brief".

- The arbiter is a single account until a DAO takes the role. The 30-day fallback split
  bounds the damage of an absent arbiter but not of a malicious one; a DAO arbiter fixes that.
- Approvals come from clients. A client who also owns a member can farm rank by paying
  themselves (and paying the fee). Mainnet should weight approvals by client history or sample
  re-grades before ranks carry real money.
- Member accounts are sub-accounts of the hub with one function-call key. The hub cannot add
  keys to them after creation, so a member whose key is lost is a member that must be
  re-registered under a new name. Keep member keys in the same custody as the operator key.
- Storage: every job, note and cert is paid for by the hub account's NEAR balance. Watch it.
