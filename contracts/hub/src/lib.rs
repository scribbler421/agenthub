//! AgentHub — certified AI agents that get hired and paid on NEAR.
//!
//! One contract, four concerns:
//! 1. Agents are sub-accounts of this contract (`name.<hub>`), created with a
//!    single function-call key scoped to `accept_job,submit,withdraw,post_note` on the hub.
//!    The agent account has *no* full-access key: nobody can drain it, not even us.
//! 2. Certs are soulbound NEP-171 tokens minted to agent accounts by the hub owner
//!    after a published benchmark round. They carry a level and an expiry.
//! 3. Jobs are funded in a NEP-141 stablecoin via `ft_transfer_call`; the budget is
//!    escrowed here and released to the agent on client approval.
//! 4. Agent earnings stay in-contract. `withdraw` enforces a per-day cap and a
//!    recipient allowlist that only the agent's human owner can change.
//!    Access keys scope *methods*; this contract enforces *policy*.

use near_sdk::json_types::U128;
use near_sdk::store::{IterableMap, LookupMap};
use near_sdk::{
    env, near, require, AccountId, Allowance, BorshStorageKey, Gas, NearToken, PanicOnDefault,
    Promise, PromiseError, PromiseOrValue, PublicKey,
};

pub type JobId = u64;
pub type CertLevel = u8; // 1 = Bronze, 2 = Silver, 3 = Gold

const NS_PER_DAY: u64 = 86_400 * 1_000_000_000;
const CERT_VALIDITY_NS: u64 = 90 * NS_PER_DAY;
const AGENT_METHODS: &str = "accept_job,submit,withdraw,post_note";
const MIN_AGENT_DEPOSIT: NearToken = NearToken::from_millinear(100); // 0.1 NEAR
const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_CALLBACK: Gas = Gas::from_tgas(10);
const GAS_MIGRATE: Gas = Gas::from_tgas(80);
const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
/// Hard ceiling on the guild fee. The owner can move the fee below this, never above.
pub const MAX_FEE_BPS: u16 = 800;
/// Smallest job the hub will escrow. 1_000_000 is 1 USDC when the token has 6 decimals.
/// Below this, a 6% fee rounds away and rank can be farmed for nothing.
pub const MIN_JOB_BUDGET: u128 = 1_000_000;
/// After submit, the client has this long to approve or dispute. Then anyone may settle.
pub const DEFAULT_REVIEW_WINDOW_NS: u64 = 5 * NS_PER_DAY;
/// After accept, the member has this long to submit. Then anyone may return the escrow.
pub const DEFAULT_ACCEPT_WINDOW_NS: u64 = 3 * NS_PER_DAY;
/// A dispute the arbiter leaves unresolved for this long may be settled by anyone at the fallback split.
pub const DISPUTE_FALLBACK_NS: u64 = 30 * NS_PER_DAY;
/// A pending refund whose callback never arrived may be sent again after this long.
pub const PENDING_REFUND_TIMEOUT_NS: u64 = NS_PER_DAY;
pub const FALLBACK_MEMBER_BPS: u16 = 5_000;
/// Journeyman requires a real client rating of at least this (x10, so 40 = 4.0).
const JOURNEYMAN_AVG10: u32 = 40;
const MAX_SKILL_LEN: usize = 64;
const MAX_SPEC_URI: usize = 256;
const MAX_OUTPUT_URI: usize = 256;
const MAX_SCORE_URI: usize = 256;
const MAX_MODEL_LEN: usize = 80;
const MAX_SKILLS: usize = 8;
const MAX_SKILL_ITEM: usize = 32;
const MAX_ALLOWLIST: usize = 8;
const MAX_SIGNER_LABEL: usize = 64;
const MAX_PROOF_FIELD: usize = 200;
/// Bytes of hub storage retained from a registration deposit, then settled in the callback.
const REGISTRATION_STORAGE_BYTES: u128 = 3_000;
/// Storage key (outside the state struct) for proposed upgrade code.
const UPGRADE_CODE_KEY: &[u8] = b"__upgrade_code";

#[derive(BorshStorageKey)]
#[near(serializers = [borsh])]
enum StorageKey {
    Agents,
    Certs,
    Jobs,
    Balances,
    WithdrawnToday,
    AgentsV2,
    JobsV2,
    TrustedSigners,
    AgentsV3,
    Revocations,
    Notes,
    AgentsV4,
    JobsV3,
    // v7 maps. Appended so existing prefixes keep their indexes.
    AgentsV5,
    JobsV4,
    StoragePrepaid,
    UsedProofs,
    SealFloor,
}

mod migrate;

pub const RANK_NAMES: [&str; 4] = ["Apprentice", "Journeyman", "Master", "Legend"];

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct AgentProfile {
    pub account: AccountId,
    pub owner: AccountId,
    pub model: String,
    pub skills: Vec<String>,
    pub jobs_completed: u32,
    pub jobs_approved: u32,
    pub total_earned: U128,
    pub withdraw_cap_per_day: U128,
    pub withdraw_allowlist: Vec<AccountId>,
    pub created_at: u64,
    /// Sum of client ratings over *rated* approvals (avg = rating_sum / rating_count).
    pub rating_sum: u32,
    pub jobs_rejected: u32,
    /// Asking price per job, token base units. Informational; the client sets the budget.
    pub rate: U128,
    /// Human-facing name shown on the board. The account name is the permanent handle.
    pub display_name: String,
    /// Approvals that carried a rating. Auto-approvals (silent client) count as approved but unrated.
    pub rating_count: u32,
    /// Rank ignores self-deals and silent auto-approvals. The counters above stay the public work record.
    pub rank_jobs_completed: u32,
    pub rank_jobs_approved: u32,
    pub rank_rating_sum: u32,
    pub rank_rating_count: u32,
}

/// Rank is computed from the account's own work history. Exams never raise it.
#[near(serializers = [json])]
pub struct Rank {
    pub rank: u8,
    pub name: String,
    pub jobs_approved: u32,
    pub approval_pct: u32,
    pub avg_rating_x10: u32,
    pub next: Option<String>,
}

/// An enclave signature over this job and the output it delivers.
/// EIP-191 text, provider_tee:
/// `job:<id>:out:<sha256 in output_uri>:<model>:<request_hash>:<response_hash>`.
/// Gateway signatures omit the model. The same signature cannot be used twice.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct ExecutionProof {
    pub model: String,
    pub chat_id: String,
    pub request_hash: String,
    pub response_hash: String,
    /// 65-byte r||s||v, hex (0x optional)
    pub signature: String,
    /// 20-byte ethereum-style address, hex (0x optional), lowercase
    pub signing_address: String,
    /// "provider_tee" or "gateway"
    pub signature_kind: String,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Cert {
    pub token_id: String,
    pub skill: String,
    pub level: CertLevel,
    pub issued_at: u64,
    pub expires_at: u64,
    pub score_uri: String,
}

/// A burned seal leaves a mark. The cert itself is gone (soulbound burn), but the
/// record of *why* stays with the member so a board can say "seal revoked after a
/// failed trial" instead of "never passed one".
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Revocation {
    pub token_id: String,
    pub skill: String,
    pub level: CertLevel,
    pub revoked_at: u64,
    pub reason: String,
}

/// A line in a contract's thread. Only the client and the assigned member may
/// write; the record is part of the contract, so a dispute reads it as-is.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub author: AccountId,
    pub text: String,
    pub at: u64,
}

const MAX_NOTES_PER_JOB: usize = 20;
const MAX_NOTE_LEN: usize = 500;

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JobStatus {
    Open,
    Accepted,
    Submitted,
    Approved,
    /// Legacy (pre-v6): client rejected and was refunded outright. New rejections open a dispute.
    Rejected,
    Cancelled,
    /// Client rejected after submit; the arbiter decides the split, or the fallback applies after 30 days.
    Disputed,
    /// Dispute settled (by the arbiter or the fallback). `member_bps` records the split.
    Resolved,
    /// Accepted, never submitted, and the accept window ran out. Escrow returned to the client.
    Expired,
}

/// Money owed back to the client, tracked separately from the business outcome so a
/// cancel and a dispute split share one refund path and nothing can be paid twice.
#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RefundStatus {
    None,
    /// Transfer sent; waiting for `on_refund`.
    Pending,
    Refunded,
    /// Transfer failed (e.g. client not registered on the token). `refund_amount` is still owed; `retry_refund`.
    Failed,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Job {
    pub id: JobId,
    pub client: AccountId,
    pub skill: String,
    pub min_level: CertLevel,
    pub budget: U128,
    pub spec_uri: String,
    pub agent: Option<AccountId>,
    pub output_uri: Option<String>,
    pub status: JobStatus,
    pub rating: Option<u8>,
    pub created_at: u64,
    pub updated_at: u64,
    /// Minimum guild rank (0..=3) the agent must hold to accept.
    pub min_rank: u8,
    /// If true, `submit` must carry a valid TEE execution proof.
    pub require_proof: bool,
    pub proof: Option<ExecutionProof>,
    /// Guild fee locked when the job was funded. A later fee change never touches this job.
    pub fee_bps: u16,
    /// After `submitted_at + review_window_ns`, anyone may `claim_auto_approve`.
    pub review_window_ns: u64,
    pub submitted_at: Option<u64>,
    pub disputed_at: Option<u64>,
    /// Share of the budget the member received when the job was settled (10000 = all of it).
    pub member_bps: Option<u16>,
    pub refund: RefundStatus,
    /// Amount owed back to the client while `refund` is Pending or Failed.
    pub refund_amount: U128,
    /// After `accepted_at + accept_window_ns`, anyone may release an accepted job that was never submitted.
    pub accept_window_ns: u64,
    pub accepted_at: Option<u64>,
    /// Bumped every time a refund transfer is sent. A late callback from an older attempt is ignored.
    pub refund_attempt: u32,
    pub refund_started_at: Option<u64>,
}

/// `msg` of the `ft_transfer_call` that funds a job.
#[near(serializers = [json])]
pub struct PostJobMsg {
    pub skill: String,
    pub min_level: CertLevel,
    pub spec_uri: String,
    pub min_rank: Option<u8>,
    pub require_proof: Option<bool>,
    /// Review window in days (1..=30). Default 5.
    pub review_days: Option<u32>,
    /// How long the member has to submit after accepting, in days (1..=30). Default 3.
    pub accept_days: Option<u32>,
}

/// Where every token the hub holds is accounted for. `total` must equal the hub's
/// balance on the token contract (minus anything someone sent without a job).
#[near(serializers = [json])]
pub struct Accounting {
    /// Budgets of jobs not yet settled (Open, Accepted, Submitted, Disputed).
    pub escrowed: U128,
    /// Members' in-contract balances.
    pub member_balances: U128,
    /// Guild fees not yet withdrawn.
    pub fees: U128,
    /// Transfers sent but not yet confirmed by their callback.
    pub in_flight: U128,
    /// Failed refunds still owed to clients.
    pub refunds_owed: U128,
    pub total: U128,
}

#[near(serializers = [json])]
pub struct Governance {
    pub owner: AccountId,
    pub pending_owner: Option<AccountId>,
    pub arbiter: AccountId,
    pub paused: bool,
    pub upgrade_delay_ns: u64,
    pub pending_upgrade: Option<(String, u64)>,
}

/// NEP-177 token metadata (subset) so wallets and explorers render certs.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct TokenMetadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub media: Option<String>,
    pub issued_at: Option<String>,
    pub expires_at: Option<String>,
    pub extra: Option<String>,
    pub reference: Option<String>,
}

#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct Token {
    pub token_id: String,
    pub owner_id: AccountId,
    pub metadata: Option<TokenMetadata>,
}

#[near(serializers = [json])]
pub struct NFTContractMetadata {
    pub spec: String,
    pub name: String,
    pub symbol: String,
    pub base_uri: Option<String>,
}

#[near(serializers = [json])]
pub struct Stats {
    pub agents: u64,
    pub jobs: u64,
    pub fee_bps: u16,
    pub token: AccountId,
    pub paused: bool,
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct AgentHub {
    owner: AccountId,
    token: AccountId,
    fee_bps: u16,
    agents: IterableMap<AccountId, AgentProfile>,
    certs: LookupMap<AccountId, Vec<Cert>>,
    jobs: IterableMap<JobId, Job>,
    next_job_id: JobId,
    balances: LookupMap<AccountId, u128>,
    withdrawn_today: LookupMap<AccountId, (u64, u128)>,
    /// Attested TEE signer addresses (lowercase hex, no 0x) → label.
    trusted_signers: IterableMap<String, String>,
    revocations: LookupMap<AccountId, Vec<Revocation>>,
    notes: LookupMap<JobId, Vec<Note>>,
    // --- v6: accounting counters. Invariant: token balance == escrowed + liabilities + in_flight + refunds_owed.
    /// Budgets held for unsettled jobs.
    escrowed: u128,
    /// Sum of every member's in-contract balance.
    liabilities: u128,
    /// Guild fees accrued and not yet withdrawn.
    fees: u128,
    /// Outgoing transfers awaiting their callback.
    in_flight: u128,
    /// Failed refunds still owed to clients.
    refunds_owed: u128,
    // --- v6: governance.
    pending_owner: Option<AccountId>,
    arbiter: AccountId,
    paused: bool,
    upgrade_delay_ns: u64,
    /// (sha256 of proposed code, proposed_at)
    pending_upgrade: Option<([u8; 32], u64)>,
    /// NEAR prepaid by an account to cover hub storage for the records they add.
    storage_prepaid: LookupMap<AccountId, u128>,
    /// Normalized proof signatures already accepted, so a signature cannot be replayed.
    used_proofs: LookupMap<String, u64>,
    /// Per member, `(skill, issued_at)` of burned seals. A seal issued at or before one of
    /// these times does not become current again.
    seal_floor: LookupMap<AccountId, Vec<(String, u64)>>,
}

/// v5 layout (before escrow hardening), read once by `migrate_v6`.
mod v5 {
    use super::*;
    #[near(serializers = [borsh])]
    pub struct AgentProfile {
        pub account: AccountId,
        pub owner: AccountId,
        pub model: String,
        pub skills: Vec<String>,
        pub jobs_completed: u32,
        pub jobs_approved: u32,
        pub total_earned: U128,
        pub withdraw_cap_per_day: U128,
        pub withdraw_allowlist: Vec<AccountId>,
        pub created_at: u64,
        pub rating_sum: u32,
        pub jobs_rejected: u32,
        pub rate: U128,
        pub display_name: String,
    }
    #[near(serializers = [borsh])]
    pub struct Job {
        pub id: JobId,
        pub client: AccountId,
        pub skill: String,
        pub min_level: CertLevel,
        pub budget: U128,
        pub spec_uri: String,
        pub agent: Option<AccountId>,
        pub output_uri: Option<String>,
        pub status: JobStatus,
        pub rating: Option<u8>,
        pub created_at: u64,
        pub updated_at: u64,
        pub min_rank: u8,
        pub require_proof: bool,
        pub proof: Option<ExecutionProof>,
    }
    #[near(serializers = [borsh])]
    pub struct State {
        pub owner: AccountId,
        pub token: AccountId,
        pub fee_bps: u16,
        pub agents: IterableMap<AccountId, AgentProfile>,
        pub certs: LookupMap<AccountId, Vec<Cert>>,
        pub jobs: IterableMap<JobId, Job>,
        pub next_job_id: JobId,
        pub balances: LookupMap<AccountId, u128>,
        pub withdrawn_today: LookupMap<AccountId, (u64, u128)>,
        pub trusted_signers: IterableMap<String, String>,
        pub revocations: LookupMap<AccountId, Vec<Revocation>>,
        pub notes: LookupMap<JobId, Vec<Note>>,
    }
}

#[near]
impl AgentHub {
    /// `token` is the NEP-141 stablecoin used for budgets (USDC on NEAR).
    /// `fee_bps` is the platform fee in basis points, capped at [`MAX_FEE_BPS`] (800 = 8%).
    #[init]
    pub fn new(owner: AccountId, token: AccountId, fee_bps: u16, upgrade_delay_ns: Option<u64>) -> Self {
        require!(fee_bps <= MAX_FEE_BPS, "fee too high");
        Self {
            owner: owner.clone(),
            token,
            fee_bps,
            agents: IterableMap::new(StorageKey::AgentsV4),
            certs: LookupMap::new(StorageKey::Certs),
            jobs: IterableMap::new(StorageKey::JobsV3),
            next_job_id: 0,
            balances: LookupMap::new(StorageKey::Balances),
            withdrawn_today: LookupMap::new(StorageKey::WithdrawnToday),
            trusted_signers: IterableMap::new(StorageKey::TrustedSigners),
            revocations: LookupMap::new(StorageKey::Revocations),
            notes: LookupMap::new(StorageKey::Notes),
            escrowed: 0, liabilities: 0, fees: 0, in_flight: 0, refunds_owed: 0,
            pending_owner: None, arbiter: owner, paused: false,
            upgrade_delay_ns: upgrade_delay_ns.unwrap_or(0), pending_upgrade: None,
            storage_prepaid: LookupMap::new(StorageKey::StoragePrepaid),
            used_proofs: LookupMap::new(StorageKey::UsedProofs),
            seal_floor: LookupMap::new(StorageKey::SealFloor),
        }
    }

    /// v5 → v6: escrow hardening. Old jobs keep the 6% fee they were settled under and get
    /// the default review window; settled refunds are marked Refunded; the owner's fee
    /// balance moves to `fees`; accounting counters are rebuilt from the records.
    /// `upgrade_delay_ns`: short on testnet; 48h is the mainnet plan (see MAINNET.md). Owner-only via #[private].
    #[private]
    #[init(ignore_state)]
    pub fn migrate_v6(upgrade_delay_ns: u64) -> Self {
        let old: v5::State = env::state_read().expect("no v5 state");
        let mut balances = old.balances;
        let mut agents = IterableMap::new(StorageKey::AgentsV4);
        let mut liabilities: u128 = 0;
        for (k, a) in old.agents.iter() {
            liabilities += balances.get(k).copied().unwrap_or(0);
            agents.insert(k.clone(), AgentProfile {
                account: a.account.clone(), owner: a.owner.clone(), model: a.model.clone(), skills: a.skills.clone(),
                jobs_completed: a.jobs_completed, jobs_approved: a.jobs_approved, total_earned: a.total_earned,
                withdraw_cap_per_day: a.withdraw_cap_per_day, withdraw_allowlist: a.withdraw_allowlist.clone(),
                created_at: a.created_at, rating_sum: a.rating_sum, jobs_rejected: a.jobs_rejected, rate: a.rate,
                display_name: a.display_name.clone(),
                rating_count: a.jobs_approved, // every approval so far carried a rating
                rank_jobs_completed: 0, rank_jobs_approved: 0, rank_rating_sum: 0, rank_rating_count: 0,
            });
        }
        let fees = balances.get(&old.owner).copied().unwrap_or(0);
        balances.remove(&old.owner);
        let mut jobs = IterableMap::new(StorageKey::JobsV3);
        let mut escrowed: u128 = 0;
        for (k, j) in old.jobs.iter() {
            let live = matches!(j.status, JobStatus::Open | JobStatus::Accepted | JobStatus::Submitted);
            if live { escrowed += j.budget.0; }
            let refunded = matches!(j.status, JobStatus::Rejected | JobStatus::Cancelled);
            jobs.insert(*k, Job {
                id: j.id, client: j.client.clone(), skill: j.skill.clone(), min_level: j.min_level, budget: j.budget,
                spec_uri: j.spec_uri.clone(), agent: j.agent.clone(), output_uri: j.output_uri.clone(), status: j.status,
                rating: j.rating, created_at: j.created_at, updated_at: j.updated_at,
                min_rank: j.min_rank, require_proof: j.require_proof, proof: j.proof.clone(),
                fee_bps: 600, review_window_ns: DEFAULT_REVIEW_WINDOW_NS,
                submitted_at: if j.status == JobStatus::Submitted { Some(j.updated_at) } else { None },
                disputed_at: None,
                member_bps: if j.status == JobStatus::Approved { Some(10_000) } else { None },
                refund: if refunded { RefundStatus::Refunded } else { RefundStatus::None },
                refund_amount: U128(0),
                accept_window_ns: DEFAULT_ACCEPT_WINDOW_NS,
                accepted_at: if j.status == JobStatus::Accepted { Some(j.updated_at) } else { None },
                refund_attempt: 0,
                refund_started_at: None,
            });
        }
        Self::recompute_rank(&mut agents, &jobs);
        env::log_str(&format!("migrated to v6: {} agents, {} jobs, escrowed {}, liabilities {}, fees {}", agents.len(), jobs.len(), escrowed, liabilities, fees));
        Self {
            owner: old.owner.clone(), token: old.token, fee_bps: old.fee_bps.min(MAX_FEE_BPS), agents, certs: old.certs, jobs,
            next_job_id: old.next_job_id, balances, withdrawn_today: old.withdrawn_today,
            trusted_signers: old.trusted_signers, revocations: old.revocations, notes: old.notes,
            escrowed, liabilities, fees, in_flight: 0, refunds_owed: 0,
            pending_owner: None, arbiter: old.owner.clone(), paused: false,
            upgrade_delay_ns, pending_upgrade: None,
            storage_prepaid: LookupMap::new(StorageKey::StoragePrepaid),
            used_proofs: LookupMap::new(StorageKey::UsedProofs),
            seal_floor: LookupMap::new(StorageKey::SealFloor),
        }
    }

    /// Read v6 (or the current layout) and write the current layout.
    /// `apply_upgrade` schedules this. The first deploy of this wasm has to call it
    /// from the hub account itself, because the previous `apply_upgrade` does not.
    /// See MAINNET.md. Already-current state is left as it is.
    #[private]
    #[init(ignore_state)]
    pub fn migrate() -> Self {
        // state_read panics when bytes exist but do not match T, so probe both layouts explicitly.
        let bytes = env::storage_read(b"STATE").expect("no state");
        if let Ok(current) = near_sdk::borsh::BorshDeserialize::try_from_slice(&bytes) {
            env::log_str("migrate: state is already current");
            return current;
        }
        let old: migrate::State = near_sdk::borsh::BorshDeserialize::try_from_slice(&bytes).unwrap_or_else(|_| env::panic_str("state is not current and not v6"));
        env::log_str("migrate: v6 -> current");
        Self::from_v6(old)
    }

    fn from_v6(mut old: migrate::State) -> Self {
        let mut agents = IterableMap::new(StorageKey::AgentsV5);
        let agent_ids: Vec<_> = old.agents.keys().cloned().collect();
        for id in agent_ids {
            let a = old.agents.remove(&id).expect("agent");
            agents.insert(id, AgentProfile {
                account: a.account, owner: a.owner, model: a.model, skills: a.skills,
                jobs_completed: a.jobs_completed, jobs_approved: a.jobs_approved, total_earned: a.total_earned,
                withdraw_cap_per_day: a.withdraw_cap_per_day, withdraw_allowlist: a.withdraw_allowlist,
                created_at: a.created_at, rating_sum: a.rating_sum, jobs_rejected: a.jobs_rejected,
                rate: a.rate, display_name: a.display_name, rating_count: a.rating_count,
                rank_jobs_completed: 0, rank_jobs_approved: 0, rank_rating_sum: 0, rank_rating_count: 0,
            });
        }
        let mut jobs = IterableMap::new(StorageKey::JobsV4);
        let mut used_proofs = LookupMap::new(StorageKey::UsedProofs);
        let job_ids: Vec<_> = old.jobs.keys().cloned().collect();
        for id in job_ids {
            let j = old.jobs.remove(&id).expect("job");
            if let Some(p) = &j.proof {
                let sig = normalize_hex(&p.signature);
                if !sig.is_empty() && !used_proofs.contains_key(&sig) {
                    used_proofs.insert(sig, j.id);
                }
            }
            jobs.insert(id, Job {
                id: j.id, client: j.client, skill: j.skill, min_level: j.min_level, budget: j.budget,
                spec_uri: j.spec_uri, agent: j.agent, output_uri: j.output_uri, status: j.status,
                rating: j.rating, created_at: j.created_at, updated_at: j.updated_at,
                min_rank: j.min_rank, require_proof: j.require_proof, proof: j.proof,
                fee_bps: j.fee_bps, review_window_ns: j.review_window_ns, submitted_at: j.submitted_at,
                disputed_at: j.disputed_at, member_bps: j.member_bps, refund: j.refund, refund_amount: j.refund_amount,
                accept_window_ns: DEFAULT_ACCEPT_WINDOW_NS,
                accepted_at: if j.status == JobStatus::Accepted { Some(j.updated_at) } else { None },
                refund_attempt: if j.refund == RefundStatus::Pending { 1 } else { 0 },
                refund_started_at: if j.refund == RefundStatus::Pending { Some(j.updated_at) } else { None },
            });
        }
        Self::recompute_rank(&mut agents, &jobs);
        Self {
            owner: old.owner, token: old.token, fee_bps: old.fee_bps.min(MAX_FEE_BPS),
            agents, certs: old.certs, jobs, next_job_id: old.next_job_id,
            balances: old.balances, withdrawn_today: old.withdrawn_today,
            trusted_signers: old.trusted_signers, revocations: old.revocations, notes: old.notes,
            escrowed: old.escrowed, liabilities: old.liabilities, fees: old.fees,
            in_flight: old.in_flight, refunds_owed: old.refunds_owed,
            pending_owner: old.pending_owner, arbiter: old.arbiter, paused: old.paused,
            upgrade_delay_ns: old.upgrade_delay_ns, pending_upgrade: old.pending_upgrade,
            storage_prepaid: LookupMap::new(StorageKey::StoragePrepaid),
            used_proofs,
            seal_floor: LookupMap::new(StorageKey::SealFloor),
        }
    }

    /// Rank is rebuilt from settled jobs so a self-deal or a silent auto-approval does not keep a farmed rank.
    fn recompute_rank(agents: &mut IterableMap<AccountId, AgentProfile>, jobs: &IterableMap<JobId, Job>) {
        for job in jobs.values() {
            let Some(agent_id) = job.agent.as_ref() else { continue };
            let Some(profile) = agents.get_mut(agent_id) else { continue };
            if job.client == profile.owner { continue; }
            let bps = job.member_bps.unwrap_or(if job.status == JobStatus::Approved { 10_000 } else { 0 });
            match job.status {
                JobStatus::Approved | JobStatus::Resolved => Self::add_rank(profile, false, bps, job.rating),
                JobStatus::Rejected => { profile.rank_jobs_completed += 1; }
                _ => {}
            }
        }
    }

    // ----------------------------------------------------------------- Agents

    /// Creates `name.<hub>` with ONE function-call key limited to
    /// `accept_job,submit,withdraw,post_note` on this contract, funds it with the attached
    /// deposit, and registers the profile once the account exists.
    /// The caller becomes the agent's human owner.
    #[payable]
    pub fn register_agent(
        &mut self,
        name: String,
        public_key: PublicKey,
        model: String,
        skills: Vec<String>,
        withdraw_cap_per_day: U128,
        withdraw_allowlist: Vec<AccountId>,
        rate: Option<U128>,
        display_name: Option<String>,
    ) -> Promise {
        let deposit = env::attached_deposit();
        let storage_held = env::storage_byte_cost().saturating_mul(REGISTRATION_STORAGE_BYTES);
        require!(
            deposit >= MIN_AGENT_DEPOSIT.saturating_add(storage_held),
            "attach at least 0.1 NEAR for the agent plus storage"
        );
        let for_agent = deposit.saturating_sub(storage_held);
        require!(
            !name.is_empty()
                && name.len() <= 32
                && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'),
            "name must be 1-32 chars of [a-z0-9-_]"
        );
        let account: AccountId = format!("{}.{}", name, env::current_account_id())
            .parse()
            .expect("invalid account id");
        require!(self.agents.get(&account).is_none(), "agent already registered");

        let profile = AgentProfile {
            account: account.clone(),
            owner: env::predecessor_account_id(),
            model,
            skills,
            jobs_completed: 0,
            jobs_approved: 0,
            total_earned: U128(0),
            withdraw_cap_per_day,
            withdraw_allowlist,
            created_at: env::block_timestamp(),
            rating_sum: 0,
            jobs_rejected: 0,
            rate: rate.unwrap_or(U128(0)),
            display_name: display_name.unwrap_or_default(),
            rating_count: 0,
            rank_jobs_completed: 0, rank_jobs_approved: 0, rank_rating_sum: 0, rank_rating_count: 0,
        };
        require!(profile.model.len() <= MAX_MODEL_LEN, "model is too long");
        require!(profile.display_name.chars().count() <= 40, "display name must be 40 chars or fewer");
        require!(profile.skills.len() <= MAX_SKILLS, "too many skills");
        require!(profile.skills.iter().all(|s| s.len() <= MAX_SKILL_ITEM && !s.contains(':')), "skill is too long");
        require!(profile.withdraw_allowlist.len() <= MAX_ALLOWLIST, "allowlist is too long");

        Promise::new(account)
            .create_account()
            .transfer(for_agent)
            .add_access_key_allowance(
                public_key,
                Allowance::unlimited(),
                env::current_account_id(),
                AGENT_METHODS.to_string(),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_CALLBACK)
                    .on_agent_created(profile, deposit, storage_held),
            )
    }

    #[private]
    pub fn on_agent_created(
        &mut self,
        profile: AgentProfile,
        deposit: NearToken,
        storage_held: NearToken,
        #[callback_result] result: Result<(), PromiseError>,
    ) -> Option<AgentProfile> {
        match result {
            Ok(()) => {
                let before = env::storage_usage();
                let account = profile.account.clone();
                self.agents.insert(account.clone(), profile.clone());
                let grew = env::storage_usage().saturating_sub(before) as u128;
                let cost = env::storage_byte_cost().saturating_mul(grew);
                let refund = storage_held.as_yoctonear().saturating_sub(cost.as_yoctonear());
                if refund > 0 {
                    let _ = Promise::new(profile.owner.clone()).transfer(NearToken::from_yoctonear(refund));
                }
                env::log_str(&format!("agent registered: {}", account));
                Some(profile)
            }
            Err(_) => {
                // The create_account batch failed atomically, so the deposit is
                // still on this contract: send it back to the caller.
                env::log_str("agent account creation failed; deposit refunded to caller");
                let _ = Promise::new(profile.owner).transfer(deposit);
                None
            }
        }
    }

    pub fn get_agent(&self, account: AccountId) -> Option<AgentProfile> {
        self.agents.get(&account).cloned()
    }

    pub fn list_agents(&self, from: Option<u64>, limit: Option<u64>) -> Vec<AgentProfile> {
        self.agents
            .values()
            .skip(from.unwrap_or(0) as usize)
            .take(limit.unwrap_or(50) as usize)
            .cloned()
            .collect()
    }

    /// Guild rank, computed from the account's history (see `rank_of`).
    pub fn get_rank(&self, agent: AccountId) -> Option<Rank> {
        self.agents.get(&agent).map(Self::rank_of)
    }

    /// Agent owner sets the name shown on the board (the account name never changes).
    pub fn set_display_name(&mut self, agent: AccountId, display_name: String) {
        require!(display_name.chars().count() <= 40, "display name must be 40 chars or fewer");
        let caller = env::predecessor_account_id();
        let before = env::storage_usage();
        {
            let p = self.agents.get_mut(&agent).expect("unknown agent");
            require!(p.owner == caller, "only the agent's owner can set the display name");
            p.display_name = display_name;
        }
        self.charge_payer(&caller, before);
    }

    /// Agent owner updates the model label (informational; shown on the board).
    pub fn set_model(&mut self, agent: AccountId, model: String) {
        require!(model.len() <= MAX_MODEL_LEN, "model is too long");
        let caller = env::predecessor_account_id();
        let before = env::storage_usage();
        {
            let p = self.agents.get_mut(&agent).expect("unknown agent");
            require!(p.owner == caller, "only the agent's owner can set the model");
            p.model = model;
        }
        self.charge_payer(&caller, before);
    }

    /// Agent owner sets the asking price.
    pub fn set_rate(&mut self, agent: AccountId, rate: U128) {
        let caller = env::predecessor_account_id();
        let p = self.agents.get_mut(&agent).expect("unknown agent");
        require!(p.owner == caller, "only the agent's owner can set the rate");
        p.rate = rate;
    }

    // ------------------------------------------------------------------ Certs

    /// Owner-only. Mints a soulbound cert to an agent after a benchmark round.
    /// `score_uri` points at the published scoring report for that round.
    pub fn mint_cert(
        &mut self,
        agent: AccountId,
        skill: String,
        level: CertLevel,
        score_uri: String,
    ) -> Cert {
        self.assert_owner();
        require!((1..=3).contains(&level), "level must be 1..=3");
        require!(!skill.is_empty() && skill.len() <= MAX_SKILL_LEN && !skill.contains(':'), "bad skill");
        require!(score_uri.len() <= MAX_SCORE_URI, "score_uri is too long");
        require!(self.agents.get(&agent).is_some(), "unknown agent");
        let payer = env::predecessor_account_id();
        let before = env::storage_usage();

        let mut certs = self.certs.get(&agent).cloned().unwrap_or_default();
        let round = certs.iter().filter(|c| c.skill == skill).count() + 1;
        let now = env::block_timestamp();
        let cert = Cert {
            token_id: format!("{}:{}:{}", skill, agent, round),
            skill,
            level,
            issued_at: now,
            expires_at: now + CERT_VALIDITY_NS,
            score_uri,
        };
        certs.push(cert.clone());
        self.certs.insert(agent.clone(), certs);

        // NEP-297 event so explorers index the mint.
        env::log_str(&format!(
            r#"EVENT_JSON:{{"standard":"nep171","version":"1.1.0","event":"nft_mint","data":[{{"owner_id":"{}","token_ids":["{}"]}}]}}"#,
            agent, cert.token_id
        ));
        self.charge_payer(&payer, before);
        cert
    }

    /// Owner-only. Revokes (burns) a cert and records why: a failed re-trial, or live
    /// approval falling below threshold.
    pub fn revoke_cert(&mut self, agent: AccountId, token_id: String, reason: Option<String>) {
        self.assert_owner();
        let payer = env::predecessor_account_id();
        let before = env::storage_usage();
        let mut certs = self.certs.get(&agent).cloned().unwrap_or_default();
        let gone = certs.iter().find(|c| c.token_id == token_id).cloned().expect("cert not found");
        certs.retain(|c| c.token_id != token_id);
        self.certs.insert(agent.clone(), certs);
        let mut revs = self.revocations.get(&agent).cloned().unwrap_or_default();
        revs.push(Revocation {
            token_id: token_id.clone(), skill: gone.skill.clone(), level: gone.level,
            revoked_at: env::block_timestamp(), reason: reason.unwrap_or_default(),
        });
        self.revocations.insert(agent.clone(), revs);
        let mut floor = self.seal_floor.get(&agent).cloned().unwrap_or_default();
        floor.push((gone.skill, gone.issued_at));
        self.seal_floor.insert(agent.clone(), floor);
        env::log_str(&format!(
            r#"EVENT_JSON:{{"standard":"nep171","version":"1.1.0","event":"nft_burn","data":[{{"owner_id":"{}","token_ids":["{}"]}}]}}"#,
            agent, token_id
        ));
        self.charge_payer(&payer, before);
    }

    pub fn get_certs(&self, agent: AccountId) -> Vec<Cert> {
        self.certs.get(&agent).cloned().unwrap_or_default()
    }

    pub fn get_revocations(&self, agent: AccountId) -> Vec<Revocation> {
        self.revocations.get(&agent).cloned().unwrap_or_default()
    }

    /// The agent's *current* seal for `skill`: the newest cert, if it is still valid.
    /// Burning that seal does not fall back to an older one. Older certs stay on the
    /// record as history and do not gate work.
    pub fn current_cert(&self, agent: AccountId, skill: String) -> Option<Cert> {
        let now = env::block_timestamp();
        let newest = self.certs.get(&agent).and_then(|certs| {
            certs.iter().filter(|c| c.skill == skill).max_by_key(|c| c.issued_at).cloned()
        })?;
        if newest.expires_at <= now {
            return None;
        }
        let blocked = self.seal_floor.get(&agent).is_some_and(|floors| {
            floors.iter().any(|(sk, issued)| sk == &skill && newest.issued_at <= *issued)
        });
        if blocked { None } else { Some(newest) }
    }

    /// True if `agent`'s current seal for `skill` is at >= `min_level`.
    pub fn is_certified(&self, agent: AccountId, skill: String, min_level: CertLevel) -> bool {
        self.current_cert(agent, skill)
            .map(|c| c.level >= min_level)
            .unwrap_or(false)
    }

    // --- NEP-171 / NEP-177 views. Transfers are intentionally refused. ---

    pub fn nft_metadata(&self) -> NFTContractMetadata {
        NFTContractMetadata {
            spec: "nft-1.0.0".into(),
            name: "AgentHub Certifications".into(),
            symbol: "AHCERT".into(),
            base_uri: None,
        }
    }

    pub fn nft_token(&self, token_id: String) -> Option<Token> {
        let agent: AccountId = token_id.split(':').nth(1)?.parse().ok()?;
        let cert = self.certs.get(&agent)?.iter().find(|c| c.token_id == token_id)?.clone();
        Some(Self::to_token(&agent, &cert))
    }

    pub fn nft_tokens_for_owner(&self, account_id: AccountId) -> Vec<Token> {
        self.certs
            .get(&account_id)
            .map(|certs| certs.iter().map(|c| Self::to_token(&account_id, c)).collect())
            .unwrap_or_default()
    }

    pub fn nft_supply_for_owner(&self, account_id: AccountId) -> U128 {
        U128(self.certs.get(&account_id).map(|c| c.len() as u128).unwrap_or(0))
    }

    #[payable]
    pub fn nft_transfer(&mut self, _receiver_id: AccountId, _token_id: String) {
        env::panic_str("certs are soulbound: they cannot be transferred");
    }

    #[payable]
    pub fn nft_transfer_call(&mut self, _receiver_id: AccountId, _token_id: String, _msg: String) {
        env::panic_str("certs are soulbound: they cannot be transferred");
    }

    // ------------------------------------------------------------------- Jobs

    /// NEP-141 receiver. Called by the token contract on `ft_transfer_call`.
    /// `msg` is JSON: {"skill":"document-extraction","min_level":2,"spec_uri":"..."}.
    /// The transferred amount becomes the escrowed job budget.
    pub fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        require!(env::predecessor_account_id() == self.token, "unsupported token");
        // Panicking here makes the token contract's ft_resolve_transfer return the funds to the sender.
        require!(!self.paused, "hub is paused: no new contracts");
        require!(amount.0 > 0, "budget must be > 0");
        require!(amount.0 >= MIN_JOB_BUDGET, "budget below minimum");
        let post: PostJobMsg = serde_json::from_str(&msg).expect("bad msg: expected {skill,min_level,spec_uri}");
        require!((1..=3).contains(&post.min_level), "min_level must be 1..=3");
        require!(!post.skill.is_empty() && post.skill.len() <= MAX_SKILL_LEN && !post.skill.contains(':'), "bad skill");
        require!(!post.spec_uri.is_empty() && post.spec_uri.len() <= MAX_SPEC_URI, "spec_uri is too long");
        let min_rank = post.min_rank.unwrap_or(0);
        require!(min_rank <= 3, "min_rank must be 0..=3");
        let review_days = post.review_days.unwrap_or(5);
        require!((1..=30).contains(&review_days), "review_days must be 1..=30");
        let accept_days = post.accept_days.unwrap_or(3);
        require!((1..=30).contains(&accept_days), "accept_days must be 1..=30");

        let id = self.next_job_id;
        self.next_job_id += 1;
        let now = env::block_timestamp();
        let job = Job {
            id,
            client: sender_id.clone(),
            skill: post.skill,
            min_level: post.min_level,
            budget: amount,
            spec_uri: post.spec_uri,
            agent: None,
            output_uri: None,
            status: JobStatus::Open,
            rating: None,
            created_at: now,
            updated_at: now,
            min_rank,
            require_proof: post.require_proof.unwrap_or(false),
            proof: None,
            fee_bps: self.fee_bps, // locked for the life of this job
            review_window_ns: review_days as u64 * NS_PER_DAY,
            submitted_at: None,
            disputed_at: None,
            member_bps: None,
            refund: RefundStatus::None,
            refund_amount: U128(0),
            accept_window_ns: accept_days as u64 * NS_PER_DAY,
            accepted_at: None,
            refund_attempt: 0,
            refund_started_at: None,
        };
        let before = env::storage_usage();
        self.jobs.insert(id, job);
        self.escrowed += amount.0;
        self.charge_payer(&sender_id, before);
        self.log_job(id, "job_posted");
        PromiseOrValue::Value(U128(0)) // keep the whole amount
    }

    /// Agent (predecessor) accepts an open job. Requires a matching, unexpired cert.
    pub fn accept_job(&mut self, job_id: JobId) {
        require!(!self.paused, "hub is paused");
        let agent = env::predecessor_account_id();
        require!(self.agents.get(&agent).is_some(), "not a registered agent");
        let (skill, min_level, min_rank) = {
            let job = self.jobs.get(&job_id).expect("no such job");
            require!(job.status == JobStatus::Open, "job is not open");
            (job.skill.clone(), job.min_level, job.min_rank)
        };
        require!(
            self.is_certified(agent.clone(), skill, min_level),
            "agent is not certified for this job"
        );
        let rank = Self::rank_of(self.agents.get(&agent).unwrap());
        require!(rank.rank >= min_rank, "agent's guild rank is too low for this job");
        let job = self.jobs.get_mut(&job_id).unwrap();
        job.agent = Some(agent);
        job.status = JobStatus::Accepted;
        job.accepted_at = Some(env::block_timestamp());
        job.updated_at = env::block_timestamp();
        self.log_job(job_id, "job_accepted");
    }

    /// Agent submits where the output lives (hash or URI), optionally with a
    /// TEE execution proof. If the job requires a proof, a missing or invalid
    /// one fails the call: the contract will not accept unverified work.
    pub fn submit(&mut self, job_id: JobId, output_uri: String, proof: Option<ExecutionProof>) {
        require!(!self.paused, "hub is paused");
        require!(output_uri.len() <= MAX_OUTPUT_URI, "output_uri is too long");
        let agent = env::predecessor_account_id();
        {
            let job = self.jobs.get(&job_id).expect("no such job");
            require!(job.status == JobStatus::Accepted, "job is not accepted");
            require!(job.agent.as_ref() == Some(&agent), "not this job's agent");
            require!(!job.require_proof || proof.is_some(), "this job requires a TEE execution proof");
        }
        let stored = proof.map(|p| self.accept_proof(job_id, &output_uri, p));
        let job = self.jobs.get_mut(&job_id).unwrap();
        job.proof = stored;
        job.output_uri = Some(output_uri);
        job.status = JobStatus::Submitted;
        job.submitted_at = Some(env::block_timestamp());
        job.updated_at = env::block_timestamp();
        self.log_job(job_id, "job_submitted");
    }

    /// Accept window elapsed and the member never submitted: send the escrow back to the client.
    pub fn release_expired_acceptance(&mut self, job_id: JobId) -> Promise {
        let (client, budget) = {
            let job = self.jobs.get_mut(&job_id).expect("no such job");
            require!(job.status == JobStatus::Accepted, "job is not accepted");
            let started = job.accepted_at.expect("no accept time");
            require!(env::block_timestamp() >= started + job.accept_window_ns, "accept window still open");
            require!(job.refund == RefundStatus::None, "refund already in progress");
            job.status = JobStatus::Expired;
            job.updated_at = env::block_timestamp();
            (job.client.clone(), job.budget.0)
        };
        self.escrowed -= budget;
        self.log_job(job_id, "job_accept_expired");
        self.start_refund(job_id, client, budget)
    }

    /// Client approves: budget minus fee is credited to the agent's in-contract
    /// balance; fee goes to the platform; rating and stats are recorded.
    /// Client or assigned member writes a line into the contract's thread.
    /// Open contracts take client notes only (no member yet); settled ones are closed.
    pub fn post_note(&mut self, job_id: JobId, text: String) {
        let job = self.jobs.get(&job_id).expect("unknown job");
        let who = env::predecessor_account_id();
        require!(who == job.client || job.agent.as_ref() == Some(&who), "only the client or the assigned member may post");
        require!(matches!(job.status, JobStatus::Open | JobStatus::Accepted | JobStatus::Submitted | JobStatus::Disputed), "contract is settled");
        // Notes are capped (20 × 500). The member's key cannot call storage_deposit, so the hub
        // covers this small record. Job text and registration are what callers pay for.
        self.push_note(job_id, who, text);
    }

    fn push_note(&mut self, job_id: JobId, author: AccountId, text: String) {
        let text = text.trim().to_string();
        require!(!text.is_empty() && text.chars().count() <= MAX_NOTE_LEN, "note must be 1..=500 characters");
        let mut notes = self.notes.get(&job_id).cloned().unwrap_or_default();
        require!(notes.len() < MAX_NOTES_PER_JOB, "thread is full");
        notes.push(Note { author: author.clone(), text: text.clone(), at: env::block_timestamp() });
        self.notes.insert(job_id, notes);
        env::log_str(&format!(
            r#"EVENT_JSON:{{"standard":"guildhall","version":"1.0.0","event":"note","data":{{"job_id":{},"author":"{}","text":{}}}}}"#,
            job_id, author, serde_json::to_string(&text).unwrap_or_default()
        ));
    }

    pub fn get_notes(&self, job_id: JobId) -> Vec<Note> {
        self.notes.get(&job_id).cloned().unwrap_or_default()
    }

    /// Client approves the submitted work. Budget minus the job's locked fee goes to the
    /// member's in-contract balance; the fee accrues to the guild; the rating is recorded.
    pub fn approve(&mut self, job_id: JobId, rating: u8) {
        require!((1..=5).contains(&rating), "rating must be 1..=5");
        let client = env::predecessor_account_id();
        {
            let job = self.jobs.get(&job_id).expect("no such job");
            require!(job.client == client, "not this job's client");
            require!(job.status == JobStatus::Submitted, "job is not submitted");
        }
        let _ = self.settle(job_id, 10_000, Some(rating), JobStatus::Approved, "job_approved"); // never refunds
    }

    /// The client said nothing for the whole review window: anyone may settle the job as
    /// approved (unrated), so a silent client cannot hold a member's pay hostage.
    pub fn claim_auto_approve(&mut self, job_id: JobId) {
        {
            let job = self.jobs.get(&job_id).expect("no such job");
            require!(job.status == JobStatus::Submitted, "job is not submitted");
            let deadline = job.submitted_at.expect("no submit time") + job.review_window_ns;
            require!(env::block_timestamp() >= deadline, "review window still open");
        }
        let _ = self.settle(job_id, 10_000, None, JobStatus::Approved, "job_auto_approved");
    }

    /// Client rejects submitted work. Nothing moves: the job becomes Disputed and the
    /// arbiter decides the split. If the arbiter sits on it for 30 days, anyone may apply
    /// the fallback split. The reason joins the thread so a dispute reads as-is.
    pub fn reject(&mut self, job_id: JobId, reason: String) {
        let client = env::predecessor_account_id();
        {
            let job = self.jobs.get_mut(&job_id).expect("no such job");
            require!(job.client == client, "not this job's client");
            require!(job.status == JobStatus::Submitted, "job is not submitted");
            job.status = JobStatus::Disputed;
            job.disputed_at = Some(env::block_timestamp());
            job.updated_at = env::block_timestamp();
        }
        if !reason.trim().is_empty() {
            self.push_note(job_id, client, format!("Rejected: {}", reason.chars().take(MAX_NOTE_LEN - 10).collect::<String>()));
        }
        self.log_job(job_id, "job_disputed");
    }

    /// Arbiter settles a dispute: `member_bps` of the budget goes to the member (fee applies
    /// to that share), the rest is refunded to the client.
    pub fn resolve_dispute(&mut self, job_id: JobId, member_bps: u16) -> PromiseOrValue<bool> {
        require!(env::predecessor_account_id() == self.arbiter, "arbiter only");
        require!(member_bps <= 10_000, "member_bps must be 0..=10000");
        {
            let job = self.jobs.get(&job_id).expect("no such job");
            require!(job.status == JobStatus::Disputed, "job is not disputed");
        }
        self.settle(job_id, member_bps, None, JobStatus::Resolved, "job_resolved")
    }

    /// Anyone: a dispute left unresolved for `DISPUTE_FALLBACK_NS` settles at the fallback split.
    pub fn claim_dispute_fallback(&mut self, job_id: JobId) -> PromiseOrValue<bool> {
        {
            let job = self.jobs.get(&job_id).expect("no such job");
            require!(job.status == JobStatus::Disputed, "job is not disputed");
            require!(env::block_timestamp() >= job.disputed_at.expect("no dispute time") + DISPUTE_FALLBACK_NS, "dispute fallback not yet available");
        }
        self.settle(job_id, FALLBACK_MEMBER_BPS, None, JobStatus::Resolved, "job_resolved_fallback")
    }

    /// Client cancels a job nobody has accepted yet; the escrow is refunded.
    pub fn cancel(&mut self, job_id: JobId) -> Promise {
        let client = env::predecessor_account_id();
        let budget = {
            let job = self.jobs.get_mut(&job_id).expect("no such job");
            require!(job.client == client, "not this job's client");
            require!(job.status == JobStatus::Open, "only open (unaccepted) jobs can be cancelled");
            job.status = JobStatus::Cancelled;
            job.updated_at = env::block_timestamp();
            job.budget.0
        };
        self.escrowed -= budget;
        self.log_job(job_id, "job_cancelled");
        self.start_refund(job_id, client, budget)
    }

    /// Re-send a refund whose transfer failed (e.g. the client was not registered on the
    /// token at the time). Client or owner.
    pub fn retry_refund(&mut self, job_id: JobId) -> Promise {
        let who = env::predecessor_account_id();
        let (client, amount) = {
            let job = self.jobs.get(&job_id).expect("no such job");
            require!(who == job.client || who == self.owner, "client or owner only");
            require!(job.refund == RefundStatus::Failed, "no failed refund on this job");
            (job.client.clone(), job.refund_amount.0)
        };
        self.refunds_owed -= amount;
        self.start_refund(job_id, client, amount)
    }

    /// The one place a settlement happens. State is final *before* any transfer leaves.
    fn settle(&mut self, job_id: JobId, member_bps: u16, rating: Option<u8>, status: JobStatus, event: &str) -> PromiseOrValue<bool> {
        let now = env::block_timestamp();
        let (agent, client, budget, fee_bps) = {
            let job = self.jobs.get_mut(&job_id).expect("no such job");
            require!(matches!(job.status, JobStatus::Submitted | JobStatus::Disputed), "job cannot be settled");
            require!(job.refund == RefundStatus::None, "refund already in progress");
            job.status = status;
            job.rating = rating;
            job.member_bps = Some(member_bps);
            job.updated_at = now;
            (job.agent.clone().expect("no member"), job.client.clone(), job.budget.0, job.fee_bps)
        };
        let member_share = budget * member_bps as u128 / 10_000;
        let client_share = budget - member_share;
        let fee = Self::guild_fee(member_share, fee_bps);
        let payout = member_share - fee;

        self.escrowed -= budget;
        self.credit(&agent, payout);
        self.fees += fee;

        let self_deal = self.agents.get(&agent).expect("agent vanished").owner == client;
        let profile = self.agents.get_mut(&agent).expect("agent vanished");
        profile.jobs_completed += 1;
        if member_bps >= 5_000 { profile.jobs_approved += 1; } else { profile.jobs_rejected += 1; }
        if let Some(r) = rating { profile.rating_sum += r as u32; profile.rating_count += 1; }
        profile.total_earned = U128(profile.total_earned.0 + payout);
        Self::add_rank(profile, self_deal, member_bps, rating);
        self.log_job(job_id, event);

        if client_share > 0 {
            PromiseOrValue::Promise(self.start_refund(job_id, client, client_share))
        } else {
            PromiseOrValue::Value(true)
        }
    }

    /// Marks the job RefundPending and sends the tokens. `on_refund` records the outcome.
    fn start_refund(&mut self, job_id: JobId, to: AccountId, amount: u128) -> Promise {
        let attempt = {
            let job = self.jobs.get_mut(&job_id).unwrap();
            job.refund = RefundStatus::Pending;
            job.refund_amount = U128(amount);
            job.refund_attempt = job.refund_attempt.saturating_add(1);
            job.refund_started_at = Some(env::block_timestamp());
            job.refund_attempt
        };
        self.in_flight += amount;
        self.ft_transfer(to, amount, format!("refund job {}", job_id)).then(
            Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_refund(job_id, attempt),
        )
    }

    /// The callback for a pending refund never arrived. Send it again after a day.
    /// The amount is already in `in_flight`, so this does not account it a second time.
    /// A callback from the older attempt is ignored. If that older transfer had actually
    /// landed, sending this one pays the client twice — only use it when the refund is stuck.
    pub fn retry_pending_refund(&mut self, job_id: JobId) -> Promise {
        let who = env::predecessor_account_id();
        let (client, amount, attempt) = {
            let job = self.jobs.get_mut(&job_id).expect("no such job");
            require!(who == job.client || who == self.owner, "client or owner only");
            require!(job.refund == RefundStatus::Pending, "no pending refund on this job");
            let started = job.refund_started_at.expect("no refund start");
            require!(env::block_timestamp() >= started + PENDING_REFUND_TIMEOUT_NS, "pending refund is still in flight");
            job.refund_attempt = job.refund_attempt.saturating_add(1);
            job.refund_started_at = Some(env::block_timestamp());
            (job.client.clone(), job.refund_amount.0, job.refund_attempt)
        };
        self.ft_transfer(client, amount, format!("refund job {} retry", job_id)).then(
            Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_refund(job_id, attempt),
        )
    }

    #[private]
    pub fn on_refund(&mut self, job_id: JobId, attempt: u32, #[callback_result] result: Result<(), PromiseError>) -> bool {
        let amount = {
            let job = self.jobs.get(&job_id).expect("no such job");
            if job.refund != RefundStatus::Pending || job.refund_attempt != attempt {
                env::log_str("stale refund callback ignored");
                return true;
            }
            job.refund_amount.0
        };
        self.in_flight -= amount;
        let job = self.jobs.get_mut(&job_id).unwrap();
        match result {
            Ok(()) => { job.refund = RefundStatus::Refunded; job.refund_amount = U128(0); self.log_job(job_id, "refund_sent"); true }
            Err(_) => { job.refund = RefundStatus::Failed; self.refunds_owed += amount; env::log_str(&format!("refund for job {} failed; {} still owed, retry_refund", job_id, amount)); false }
        }
    }

    pub fn get_job(&self, job_id: JobId) -> Option<Job> {
        self.jobs.get(&job_id).cloned()
    }

    pub fn list_jobs(&self, status: Option<JobStatus>, from: Option<u64>, limit: Option<u64>) -> Vec<Job> {
        self.jobs
            .values()
            .filter(|j| status.as_ref().map(|s| &j.status == s).unwrap_or(true))
            .skip(from.unwrap_or(0) as usize)
            .take(limit.unwrap_or(50) as usize)
            .cloned()
            .collect()
    }

    // --------------------------------------------------- Spending policy

    /// Agent withdraws earned tokens. Policy is enforced HERE, not by the key:
    /// - `to` must be in the agent's allowlist
    /// - amount + already withdrawn today must be <= withdraw_cap_per_day
    pub fn withdraw(&mut self, to: AccountId, amount: U128) -> Promise {
        require!(!self.paused, "hub is paused");
        let agent = env::predecessor_account_id();
        let amount = amount.0;
        require!(amount > 0, "amount must be > 0");
        let profile = self.agents.get(&agent).expect("not a registered agent");
        require!(profile.withdraw_allowlist.contains(&to), "recipient not in allowlist");

        let today = env::block_timestamp() / NS_PER_DAY;
        let spent = match self.withdrawn_today.get(&agent) {
            Some((day, spent)) if *day == today => *spent,
            _ => 0,
        };
        require!(
            spent + amount <= profile.withdraw_cap_per_day.0,
            "exceeds daily withdrawal cap"
        );

        let balance = self.balances.get(&agent).copied().unwrap_or(0);
        require!(balance >= amount, "insufficient balance");
        self.balances.insert(agent.clone(), balance - amount);
        self.liabilities -= amount;
        self.in_flight += amount;
        self.withdrawn_today.insert(agent.clone(), (today, spent + amount));

        self.ft_transfer(to, amount, format!("withdrawal by {}", agent)).then(
            Self::ext(env::current_account_id())
                .with_static_gas(GAS_CALLBACK)
                .on_withdraw(agent, U128(amount)),
        )
    }

    #[private]
    pub fn on_withdraw(
        &mut self,
        agent: AccountId,
        amount: U128,
        #[callback_result] result: Result<(), PromiseError>,
    ) -> bool {
        self.in_flight -= amount.0;
        if result.is_err() {
            // Token transfer failed (e.g. recipient not registered on the token):
            // put the funds and the daily allowance back.
            self.credit(&agent, amount.0);
            if let Some((day, spent)) = self.withdrawn_today.get(&agent).copied() {
                self.withdrawn_today.insert(agent, (day, spent.saturating_sub(amount.0)));
            }
            env::log_str("withdrawal failed; balance restored");
            return false;
        }
        true
    }

    /// Only the agent's human owner can change its policy. The agent's key
    /// cannot call this method at all (it is not in the key's method list).
    pub fn set_policy(
        &mut self,
        agent: AccountId,
        withdraw_cap_per_day: U128,
        withdraw_allowlist: Vec<AccountId>,
    ) {
        let caller = env::predecessor_account_id();
        require!(withdraw_allowlist.len() <= MAX_ALLOWLIST, "allowlist is too long");
        let before = env::storage_usage();
        {
            let profile = self.agents.get_mut(&agent).expect("unknown agent");
            require!(profile.owner == caller, "only the agent's owner can set policy");
            profile.withdraw_cap_per_day = withdraw_cap_per_day;
            profile.withdraw_allowlist = withdraw_allowlist;
        }
        self.charge_payer(&caller, before);
    }

    /// Owner withdraws accrued guild fees. Debited first; restored by the callback on failure.
    pub fn withdraw_fees(&mut self, to: AccountId, amount: U128) -> Promise {
        self.assert_owner();
        require!(!self.paused, "hub is paused");
        require!(amount.0 > 0 && self.fees >= amount.0, "insufficient fee balance");
        self.fees -= amount.0;
        self.in_flight += amount.0;
        self.ft_transfer(to, amount.0, "guild fees".into()).then(
            Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_withdraw_fees(amount),
        )
    }

    #[private]
    pub fn on_withdraw_fees(&mut self, amount: U128, #[callback_result] result: Result<(), PromiseError>) -> bool {
        self.in_flight -= amount.0;
        if result.is_err() {
            self.fees += amount.0;
            env::log_str("fee withdrawal failed; fees restored");
            return false;
        }
        true
    }

    // -------------------------------------------------------------- Governance

    /// Two-step ownership transfer: the proposed account must call `accept_owner`, so a
    /// typo or an unready multisig cannot orphan the hub.
    pub fn propose_owner(&mut self, new_owner: AccountId) {
        self.assert_owner();
        self.pending_owner = Some(new_owner);
    }

    pub fn accept_owner(&mut self) {
        let who = env::predecessor_account_id();
        require!(self.pending_owner.as_ref() == Some(&who), "not the proposed owner");
        env::log_str(&format!("owner {} -> {}", self.owner, who));
        if self.arbiter == self.owner { self.arbiter = who.clone(); }
        self.owner = who;
        self.pending_owner = None;
    }

    /// Who settles disputes. Defaults to the owner; can be a separate account or a DAO.
    pub fn set_arbiter(&mut self, arbiter: AccountId) {
        self.assert_owner();
        self.arbiter = arbiter;
    }

    /// Pause stops money coming in and going out to members (funding, accepts, submits,
    /// withdrawals). Refunds, approvals and dispute settlement keep working so nobody's
    /// funds are trapped by the pause itself.
    pub fn set_paused(&mut self, paused: bool) {
        self.assert_owner();
        self.paused = paused;
        env::log_str(if paused { "hub paused" } else { "hub unpaused" });
    }

    /// Stage new contract code. It can only be deployed by `apply_upgrade` after the delay.
    /// Meaningful once the hub account holds no full-access key (see MAINNET.md).
    pub fn propose_upgrade(&mut self, code: Vec<u8>) {
        self.assert_owner();
        require!(!code.is_empty(), "empty code");
        let hash = env::sha256_array(&code);
        env::storage_write(UPGRADE_CODE_KEY, &code);
        self.pending_upgrade = Some((hash, env::block_timestamp()));
        env::log_str(&format!("upgrade proposed: sha256 {} ({} bytes), applicable after {} s", hex_encode(&hash), code.len(), self.upgrade_delay_ns / 1_000_000_000));
    }

    pub fn cancel_upgrade(&mut self) {
        self.assert_owner();
        self.pending_upgrade = None;
        env::storage_remove(UPGRADE_CODE_KEY);
    }

    pub fn apply_upgrade(&mut self) -> Promise {
        self.assert_owner();
        let (hash, proposed_at) = self.pending_upgrade.expect("no upgrade proposed");
        require!(env::block_timestamp() >= proposed_at + self.upgrade_delay_ns, "upgrade delay has not passed");
        let code = env::storage_read(UPGRADE_CODE_KEY).expect("upgrade code missing");
        require!(env::sha256_array(&code) == hash, "upgrade code does not match proposal");
        self.pending_upgrade = None;
        env::storage_remove(UPGRADE_CODE_KEY);
        env::log_str(&format!("applying upgrade sha256 {}", hex_encode(&hash)));
        Promise::new(env::current_account_id()).deploy_contract(code).then(
            Self::ext(env::current_account_id()).with_static_gas(GAS_MIGRATE).migrate(),
        )
    }

    /// The delay can be lengthened at any time; shortening it takes an upgrade, i.e. the delay itself.
    pub fn set_upgrade_delay(&mut self, upgrade_delay_ns: u64) {
        self.assert_owner();
        require!(upgrade_delay_ns >= self.upgrade_delay_ns, "the upgrade delay can only grow");
        self.upgrade_delay_ns = upgrade_delay_ns;
    }

    pub fn get_governance(&self) -> Governance {
        Governance {
            owner: self.owner.clone(), pending_owner: self.pending_owner.clone(), arbiter: self.arbiter.clone(),
            paused: self.paused, upgrade_delay_ns: self.upgrade_delay_ns,
            pending_upgrade: self.pending_upgrade.map(|(h, t)| (hex_encode(&h), t)),
        }
    }

    /// Where every token is. `total` should match the hub's balance on the token contract.
    pub fn get_accounting(&self) -> Accounting {
        Accounting {
            escrowed: U128(self.escrowed), member_balances: U128(self.liabilities), fees: U128(self.fees),
            in_flight: U128(self.in_flight), refunds_owed: U128(self.refunds_owed),
            total: U128(self.escrowed + self.liabilities + self.fees + self.in_flight + self.refunds_owed),
        }
    }

    // ------------------------------------------------ Trusted TEE signers

    /// Owner registers a signer address after verifying its attestation
    /// off-chain (NEAR AI Cloud attestation report + NVIDIA/Intel evidence).
    pub fn add_trusted_signer(&mut self, address: String, label: String) {
        self.assert_owner();
        require!(label.len() <= MAX_SIGNER_LABEL, "label is too long");
        let a = normalize_hex(&address);
        require!(a.len() == 40, "address must be 20 bytes hex");
        let payer = env::predecessor_account_id();
        let before = env::storage_usage();
        self.trusted_signers.insert(a, label);
        self.charge_payer(&payer, before);
    }

    pub fn remove_trusted_signer(&mut self, address: String) {
        self.assert_owner();
        self.trusted_signers.remove(&normalize_hex(&address));
    }

    pub fn list_trusted_signers(&self) -> Vec<(String, String)> {
        self.trusted_signers.iter().map(|(k, v)| (format!("0x{}", k), v.clone())).collect()
    }

    /// Pure check: signature binds `job_id` and `output_hash`, recovers to a trusted signer, and is unused.
    pub fn verify_proof(&self, proof: ExecutionProof, job_id: JobId, output_hash: String) -> (bool, String) {
        let output_hash = normalize_hex(&output_hash);
        if output_hash.len() != 64 {
            return (false, "output hash must be 64 hex chars".into());
        }
        let sig = normalize_hex(&proof.signature);
        if self.used_proofs.contains_key(&sig) {
            return (false, "proof already used".into());
        }
        let signer = Self::recover_signer(&proof, job_id, &output_hash);
        if signer != normalize_hex(&proof.signing_address) {
            return (false, "signature does not recover to signing_address".into());
        }
        match self.trusted_signers.get(&signer) {
            Some(label) => (true, format!("signed by attested TEE {} ({})", proof.signing_address, label)),
            None => (false, "signer is not a trusted TEE on this hub".into()),
        }
    }

    pub fn get_balance(&self, account: AccountId) -> U128 {
        U128(self.balances.get(&account).copied().unwrap_or(0))
    }

    /// Owner-only. Changes the guild's cut for contracts funded *after* this call. Every
    /// existing job keeps the fee it was funded under. Hard-capped at MAX_FEE_BPS.
    pub fn set_fee_bps(&mut self, fee_bps: u16) {
        self.assert_owner();
        require!(fee_bps <= MAX_FEE_BPS, "fee too high");
        env::log_str(&format!(
            r#"EVENT_JSON:{{"standard":"guildhall","version":"1.0.0","event":"fee_changed","data":{{"from_bps":{},"to_bps":{}}}}}"#,
            self.fee_bps, fee_bps
        ));
        self.fee_bps = fee_bps;
    }

    pub fn get_stats(&self) -> Stats {
        Stats {
            agents: self.agents.len() as u64,
            jobs: self.jobs.len() as u64,
            fee_bps: self.fee_bps,
            token: self.token.clone(),
            paused: self.paused,
        }
    }

    // ---------------------------------------------------------------- Helpers

    pub(crate) fn rank_of(p: &AgentProfile) -> Rank {
        let approved = p.rank_jobs_approved;
        let pct = if p.rank_jobs_completed > 0 { approved * 100 / p.rank_jobs_completed } else { 0 };
        let avg10 = if p.rank_rating_count > 0 { p.rank_rating_sum * 10 / p.rank_rating_count } else { 0 };
        let rank: u8 = if approved >= 100 && pct >= 97 && avg10 >= 47 { 3 }
            else if approved >= 25 && pct >= 95 && avg10 >= 45 { 2 }
            else if approved >= 5 && pct >= 90 && avg10 >= JOURNEYMAN_AVG10 { 1 }
            else { 0 };
        let next = match rank {
            0 => Some(format!("Journeyman: {} more approved jobs, 90%+ approval, 4.0+ rating", 5u32.saturating_sub(approved))),
            1 => Some(format!("Master: {} more approved jobs, 95%+ approval, 4.5+ rating", 25u32.saturating_sub(approved))),
            2 => Some(format!("Legend: {} more approved jobs, 97%+ approval, 4.7+ rating", 100u32.saturating_sub(approved))),
            _ => None,
        };
        Rank { rank, name: RANK_NAMES[rank as usize].into(), jobs_approved: approved, approval_pct: pct, avg_rating_x10: avg10, next }
    }

    /// Self-deals and silent auto-approvals pay out but do not move rank.
    /// An unrated loss (a dispute the member mostly loses) does lower the approval rate.
    fn add_rank(profile: &mut AgentProfile, self_deal: bool, member_bps: u16, rating: Option<u8>) {
        if self_deal { return; }
        match rating {
            Some(r) => {
                profile.rank_jobs_completed += 1;
                if member_bps >= FALLBACK_MEMBER_BPS { profile.rank_jobs_approved += 1; }
                profile.rank_rating_sum += r as u32;
                profile.rank_rating_count += 1;
            }
            None if member_bps >= FALLBACK_MEMBER_BPS => {}
            None => { profile.rank_jobs_completed += 1; }
        }
    }

    /// Fee in token units, rounded up so a tiny job cannot zero it out.
    fn guild_fee(member_share: u128, fee_bps: u16) -> u128 {
        if member_share == 0 || fee_bps == 0 { return 0; }
        let fee = member_share.checked_mul(fee_bps as u128).expect("fee overflow").div_ceil(10_000);
        require!(fee <= member_share, "fee exceeds member share");
        fee
    }

    /// EIP-191 recovery. The signed text binds the job id and the output hash.
    fn recover_signer(p: &ExecutionProof, job_id: JobId, output_hash: &str) -> String {
        let text = if p.signature_kind == "provider_tee" {
            format!("job:{job_id}:out:{output_hash}:{}:{}:{}", p.model, p.request_hash, p.response_hash)
        } else {
            format!("job:{job_id}:out:{output_hash}:{}:{}", p.request_hash, p.response_hash)
        };
        let mut msg = format!("\x19Ethereum Signed Message:\n{}", text.len()).into_bytes();
        msg.extend_from_slice(text.as_bytes());
        let hash = env::keccak256_array(&msg);
        let sig = hex_decode(&normalize_hex(&p.signature));
        require!(sig.len() == 65, "proof: signature must be 65 bytes");
        let mut v = sig[64];
        if v >= 27 { v -= 27; }
        let pubkey = env::ecrecover(&hash, &sig[..64], v, false).expect("proof: ecrecover failed");
        let h = env::keccak256_array(&pubkey);
        hex_encode(&h[12..])
    }

    fn assert_owner(&self) {
        require!(env::predecessor_account_id() == self.owner, "owner only");
    }

    fn credit(&mut self, account: &AccountId, amount: u128) {
        let bal = self.balances.get(account).copied().unwrap_or(0);
        self.balances.insert(account.clone(), bal + amount);
        self.liabilities += amount;
    }

    /// Caller prepays NEAR that stays on the hub and pays for the storage their records add.
    #[payable]
    pub fn storage_deposit(&mut self) {
        let amount = env::attached_deposit().as_yoctonear();
        require!(amount > 0, "attach NEAR for storage");
        let who = env::predecessor_account_id();
        let before = env::storage_usage();
        let cur = self.storage_prepaid.get(&who).copied().unwrap_or(0);
        self.storage_prepaid.insert(who.clone(), cur.saturating_add(amount));
        self.charge_payer(&who, before);
    }

    pub fn get_storage_balance(&self, account: AccountId) -> U128 {
        U128(self.storage_prepaid.get(&account).copied().unwrap_or(0))
    }

    fn charge_payer(&mut self, payer: &AccountId, before: u64) {
        let after = env::storage_usage();
        if after <= before { return; }
        let cost = env::storage_byte_cost().as_yoctonear().saturating_mul((after - before) as u128);
        let cur = self.storage_prepaid.get(payer).copied().unwrap_or(0);
        require!(cur >= cost, "insufficient storage deposit; call storage_deposit");
        self.storage_prepaid.insert(payer.clone(), cur - cost);
    }

    fn accept_proof(&mut self, job_id: JobId, output_uri: &str, p: ExecutionProof) -> ExecutionProof {
        require!(p.model.len() <= MAX_PROOF_FIELD && p.chat_id.len() <= MAX_PROOF_FIELD, "proof field is too long");
        require!(p.request_hash.len() <= MAX_PROOF_FIELD && p.response_hash.len() <= MAX_PROOF_FIELD, "proof field is too long");
        require!(p.signature_kind.len() <= 32 && p.signing_address.len() <= 44, "proof field is too long");
        let output_hash = output_hash_of(output_uri);
        let sig = normalize_hex(&p.signature);
        require!(sig.len() == 130, "proof: signature must be 65 bytes");
        require!(!self.used_proofs.contains_key(&sig), "proof already used");
        let signer = Self::recover_signer(&p, job_id, &output_hash);
        require!(signer == normalize_hex(&p.signing_address), "proof: signature does not recover to signing_address");
        require!(self.trusted_signers.contains_key(&signer), "proof: signer is not an attested TEE registered on this hub");
        self.used_proofs.insert(sig, job_id);
        p
    }

    fn ft_transfer(&self, receiver: AccountId, amount: u128, memo: String) -> Promise {
        let args = serde_json::json!({
            "receiver_id": receiver,
            "amount": U128(amount),
            "memo": memo,
        });
        Promise::new(self.token.clone()).function_call(
            "ft_transfer".to_string(),
            args.to_string().into_bytes(),
            ONE_YOCTO,
            GAS_FT_TRANSFER,
        )
    }

    fn to_token(agent: &AccountId, c: &Cert) -> Token {
        let level = match c.level {
            3 => "Gold",
            2 => "Silver",
            _ => "Bronze",
        };
        Token {
            token_id: c.token_id.clone(),
            owner_id: agent.clone(),
            metadata: Some(TokenMetadata {
                title: Some(format!("{} — {}", c.skill, level)),
                description: Some(format!(
                    "AgentHub certification: {} at {} level. Soulbound; expires {} days after issue.",
                    c.skill, level, CERT_VALIDITY_NS / NS_PER_DAY
                )),
                media: None,
                issued_at: Some((c.issued_at / 1_000_000).to_string()),
                expires_at: Some((c.expires_at / 1_000_000).to_string()),
                extra: Some(format!(r#"{{"level":{},"skill":"{}"}}"#, c.level, c.skill)),
                reference: Some(c.score_uri.clone()),
            }),
        }
    }

    fn log_job(&self, job_id: JobId, event: &str) {
        env::log_str(&format!(
            r#"EVENT_JSON:{{"standard":"agenthub","version":"1.0.0","event":"{}","data":{{"job_id":{}}}}}"#,
            event, job_id
        ));
    }
}

fn normalize_hex(s: &str) -> String {
    s.trim().trim_start_matches("0x").trim_start_matches("0X").to_ascii_lowercase()
}

fn output_hash_of(uri: &str) -> String {
    let Some((_, hash)) = uri.rsplit_once("#sha256=") else {
        env::panic_str("output_uri must include #sha256=<64 hex>");
    };
    let h = normalize_hex(hash);
    require!(h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()), "output hash must be 64 hex chars");
    h
}

fn hex_decode(s: &str) -> Vec<u8> {
    require!(s.len() % 2 == 0, "bad hex");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("bad hex")).collect()
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(unused_must_use)]
mod tests {
    use super::*;
    use near_sdk::test_utils::{accounts, VMContextBuilder};
    use near_sdk::testing_env;

    fn hub() -> AccountId {
        "hub.testnet".parse().unwrap()
    }
    fn token() -> AccountId {
        "usdc.testnet".parse().unwrap()
    }
    fn agent() -> AccountId {
        "extractor.hub.testnet".parse().unwrap()
    }

    fn ctx(predecessor: AccountId) -> VMContextBuilder {
        let mut b = VMContextBuilder::new();
        b.current_account_id(hub())
            .predecessor_account_id(predecessor)
            .block_timestamp(1_000 * NS_PER_DAY);
        b
    }

    fn setup() -> AgentHub {
        testing_env!(ctx(accounts(0)).build());
        let mut c = AgentHub::new(accounts(0), token(), 600, Some(60 * 1_000_000_000));
        // Register the agent directly (skipping the cross-contract account creation).
        c.agents.insert(
            agent(),
            AgentProfile {
                account: agent(),
                owner: accounts(1),
                model: "test-model".into(),
                skills: vec!["document-extraction".into()],
                jobs_completed: 0,
                jobs_approved: 0,
                total_earned: U128(0),
                withdraw_cap_per_day: U128(3_000_000), // 3 USDC/day
                withdraw_allowlist: vec![accounts(1)],
                created_at: 0,
                rating_sum: 0,
                jobs_rejected: 0,
                rate: U128(0),
                display_name: "Ledger Hand".into(),
                rating_count: 0,
                rank_jobs_completed: 0, rank_jobs_approved: 0, rank_rating_sum: 0, rank_rating_count: 0,
            },
        );
        // Tests don't go through storage_deposit. Credit the accounts that write records.
        for id in [accounts(0), accounts(1), accounts(2), accounts(5), agent()] {
            c.storage_prepaid.insert(id, 50 * 10u128.pow(24));
        }
        c
    }

    /// What the token contract holds for the hub, assuming transfers in flight have not landed yet.
    /// Every test calls `check(&c, token_balance)` after each operation.
    fn check(c: &AgentHub, token_balance: u128) {
        let a = c.get_accounting();
        assert_eq!(a.total.0, token_balance, "accounting drift: escrowed {} + members {} + fees {} + in_flight {} + owed {} != token {}",
            a.escrowed.0, a.member_balances.0, a.fees.0, a.in_flight.0, a.refunds_owed.0, token_balance);
    }

    fn gold(c: &mut AgentHub) {
        testing_env!(ctx(accounts(0)).build());
        c.mint_cert(agent(), "document-extraction".into(), 3, "".into());
    }

    fn submit_job(c: &mut AgentHub, id: JobId) {
        testing_env!(ctx(agent()).build());
        c.accept_job(id);
        c.submit(id, "ipfs://output".into(), None);
    }

    fn post_job(c: &mut AgentHub, client: AccountId, budget: u128, min_level: u8) -> JobId {
        testing_env!(ctx(token()).build());
        c.ft_on_transfer(
            client,
            U128(budget),
            format!(r#"{{"skill":"document-extraction","min_level":{},"spec_uri":"ipfs://spec"}}"#, min_level),
        );
        c.next_job_id - 1
    }

    #[test]
    fn job_lifecycle_pays_agent_minus_fee() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).build());
        c.mint_cert(agent(), "document-extraction".into(), 2, "ipfs://report".into());
        assert!(c.is_certified(agent(), "document-extraction".into(), 2));
        assert!(!c.is_certified(agent(), "document-extraction".into(), 3));

        let id = post_job(&mut c, accounts(2), 5_000_000, 2);
        assert_eq!(c.get_job(id).unwrap().status, JobStatus::Open);

        testing_env!(ctx(agent()).build());
        c.accept_job(id);
        c.submit(id, "ipfs://output".into(), None);

        check(&c, 5_000_000);
        testing_env!(ctx(accounts(2)).build());
        c.approve(id, 5);

        assert_eq!(c.get_balance(agent()).0, 4_700_000);
        assert_eq!(c.get_accounting().fees.0, 300_000);
        let p = c.get_agent(agent()).unwrap();
        assert_eq!(p.jobs_approved, 1);
        assert_eq!(p.rating_count, 1);
        assert_eq!(p.total_earned.0, 4_700_000);
        check(&c, 5_000_000);
    }

    #[test]
    #[should_panic(expected = "not certified")]
    fn uncertified_agent_cannot_accept() {
        let mut c = setup();
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        testing_env!(ctx(agent()).build());
        c.accept_job(id);
    }

    #[test]
    #[should_panic(expected = "not certified")]
    fn expired_cert_does_not_count() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).build());
        c.mint_cert(agent(), "document-extraction".into(), 3, "".into());
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        // 91 days later
        testing_env!(ctx(agent()).block_timestamp(1_091 * NS_PER_DAY).build());
        c.accept_job(id);
    }

    #[test]
    fn latest_seal_replaces_earlier_one() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).block_timestamp(1).build());
        c.mint_cert(agent(), "document-extraction".into(), 3, "".into());
        assert!(c.is_certified(agent(), "document-extraction".into(), 3));
        // A later, worse trial is the agent's seal now — Gold does not linger.
        testing_env!(ctx(accounts(0)).block_timestamp(2).build());
        c.mint_cert(agent(), "document-extraction".into(), 1, "".into());
        assert!(!c.is_certified(agent(), "document-extraction".into(), 2));
        assert!(c.is_certified(agent(), "document-extraction".into(), 1));
        assert_eq!(c.current_cert(agent(), "document-extraction".into()).unwrap().level, 1);
        assert_eq!(c.get_certs(agent()).len(), 2, "history is kept");
        // Burning the current (Bronze) seal must not restore the older Gold.
        testing_env!(ctx(accounts(0)).block_timestamp(3).build());
        c.revoke_cert(agent(), "document-extraction:extractor.hub.testnet:2".into(), Some("failed re-trial".into()));
        assert!(c.current_cert(agent(), "document-extraction".into()).is_none(), "older gold must stay history");
        assert!(!c.is_certified(agent(), "document-extraction".into(), 1));
        assert_eq!(c.get_certs(agent()).len(), 1, "the gold seal is still on the record");
        assert_eq!(c.get_certs(agent())[0].level, 3);
        let revs = c.get_revocations(agent());
        assert_eq!(revs.len(), 1);
        assert_eq!(revs[0].reason, "failed re-trial");
        assert_eq!(revs[0].level, 1);
    }

    #[test]
    fn notes_are_for_the_two_parties_only() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).build());
        c.mint_cert(agent(), "document-extraction".into(), 3, "".into());
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        testing_env!(ctx(accounts(2)).build());
        c.post_note(id, "Only the first document counts.".into());
        testing_env!(ctx(agent()).build());
        c.accept_job(id);
        c.post_note(id, "Understood; the second page is ignored.".into());
        assert_eq!(c.get_notes(id).len(), 2);
        assert_eq!(c.get_notes(id)[1].author, agent());
    }

    #[test]
    #[should_panic(expected = "only the client or the assigned member")]
    fn strangers_cannot_post_notes() {
        let mut c = setup();
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        testing_env!(ctx(accounts(5)).build());
        c.post_note(id, "hello".into());
    }

    #[test]
    fn owner_can_change_fee() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).build());
        c.set_fee_bps(600);
        assert_eq!(c.get_stats().fee_bps, 600);
    }

    #[test]
    #[should_panic(expected = "fee too high")]
    fn fee_is_capped() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).build());
        c.set_fee_bps(MAX_FEE_BPS + 1);
    }

    #[test]
    #[should_panic(expected = "unsupported token")]
    fn rejects_other_tokens() {
        let mut c = setup();
        testing_env!(ctx(accounts(3)).build());
        c.ft_on_transfer(accounts(2), U128(1), "{}".into());
    }

    #[test]
    fn withdraw_enforces_allowlist_and_cap() {
        let mut c = setup();
        c.credit(&agent(), 10_000_000);
        check(&c, 10_000_000);

        testing_env!(ctx(agent()).build());
        c.withdraw(accounts(1), U128(2_000_000));
        assert_eq!(c.get_balance(agent()).0, 8_000_000);
        check(&c, 10_000_000); // in flight until the callback
        c.on_withdraw(agent(), U128(2_000_000), Ok(()));
        check(&c, 8_000_000);

        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            testing_env!(ctx(agent()).build());
            c.withdraw(accounts(1), U128(1_500_000)); // 3.5 > 3 cap
        }));
        assert!(r.is_err(), "cap should be enforced");

        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            testing_env!(ctx(agent()).build());
            c.withdraw(accounts(4), U128(100)); // not allowlisted
        }));
        assert!(r.is_err(), "allowlist should be enforced");

        // Next day the cap resets.
        testing_env!(ctx(agent()).block_timestamp(1_001 * NS_PER_DAY).build());
        c.withdraw(accounts(1), U128(3_000_000));
        assert_eq!(c.get_balance(agent()).0, 5_000_000);
        // A failed transfer restores balance, allowance and accounting.
        c.on_withdraw(agent(), U128(3_000_000), Err(PromiseError::Failed));
        assert_eq!(c.get_balance(agent()).0, 8_000_000);
        check(&c, 8_000_000);
    }

    #[test]
    #[should_panic(expected = "only the agent's owner")]
    fn agent_cannot_change_own_policy() {
        let mut c = setup();
        testing_env!(ctx(agent()).build());
        c.set_policy(agent(), U128(u128::MAX), vec![accounts(5)]);
    }

    #[test]
    fn owner_can_change_policy() {
        let mut c = setup();
        testing_env!(ctx(accounts(1)).build());
        c.set_policy(agent(), U128(1), vec![accounts(5)]);
        let p = c.get_agent(agent()).unwrap();
        assert_eq!(p.withdraw_cap_per_day.0, 1);
        assert_eq!(p.withdraw_allowlist, vec![accounts(5)]);
    }

    #[test]
    #[should_panic(expected = "soulbound")]
    fn certs_cannot_be_transferred() {
        let mut c = setup();
        testing_env!(ctx(agent()).attached_deposit(ONE_YOCTO).build());
        c.nft_transfer(accounts(5), "document-extraction:extractor.hub.testnet:1".into());
    }

    #[test]
    fn nft_views_render_certs() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).build());
        let cert = c.mint_cert(agent(), "document-extraction".into(), 3, "ipfs://r".into());
        let t = c.nft_token(cert.token_id.clone()).unwrap();
        assert_eq!(t.owner_id, agent());
        assert!(t.metadata.unwrap().title.unwrap().contains("Gold"));
        assert_eq!(c.nft_tokens_for_owner(agent()).len(), 1);
        assert_eq!(c.nft_supply_for_owner(agent()).0, 1);
    }

    #[test]
    fn ranks_follow_the_record() {
        let mut c = setup();
        let p = c.agents.get_mut(&agent()).unwrap();
        assert_eq!(AgentHub::rank_of(p).name, "Apprentice");
        p.rank_jobs_completed = 5; p.rank_jobs_approved = 5; p.rank_rating_sum = 20; p.rank_rating_count = 5;
        assert_eq!(AgentHub::rank_of(p).name, "Journeyman");
        // Five approvals with no real rating are not Journeyman.
        p.rank_rating_sum = 0; p.rank_rating_count = 0;
        assert_eq!(AgentHub::rank_of(p).rank, 0);
        p.rank_rating_sum = 15; p.rank_rating_count = 5; // 3.0 avg
        assert_eq!(AgentHub::rank_of(p).rank, 0, "Journeyman needs a 4.0 rating");
        p.rank_rating_sum = 20; p.rank_rating_count = 5;
        p.rank_jobs_completed = 26; p.rank_jobs_approved = 25; p.rank_rating_sum = 115; p.rank_rating_count = 25; // 4.6 avg, 96%
        assert_eq!(AgentHub::rank_of(p).name, "Master");
        p.rank_jobs_completed = 100; p.rank_jobs_approved = 100; p.rank_rating_sum = 480; p.rank_rating_count = 100;
        assert_eq!(AgentHub::rank_of(p).name, "Legend");
        // Unrated approvals are not in the rank counters, so they do not move the average.
        assert_eq!(AgentHub::rank_of(p).avg_rating_x10, 48);
        p.rank_jobs_completed = 10; p.rank_jobs_approved = 8; p.rank_rating_sum = 40; p.rank_rating_count = 8; // 80%
        assert_eq!(AgentHub::rank_of(p).rank, 0);
    }

    #[test]
    #[should_panic(expected = "guild rank is too low")]
    fn rank_gates_jobs() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).build());
        c.mint_cert(agent(), "document-extraction".into(), 3, "".into());
        testing_env!(ctx(token()).build());
        c.ft_on_transfer(accounts(2), U128(1_000_000), r#"{"skill":"document-extraction","min_level":1,"spec_uri":"s","min_rank":2}"#.into());
        testing_env!(ctx(agent()).build());
        c.accept_job(0);
    }

    // ------------------------------------------------ escrow hardening (v6)

    #[test]
    fn fee_is_locked_per_job() {
        let mut c = setup(); gold(&mut c);
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        testing_env!(ctx(accounts(0)).build());
        c.set_fee_bps(800); // raised after funding
        let id2 = post_job(&mut c, accounts(2), 1_000_000, 1);
        assert_eq!(c.get_job(id).unwrap().fee_bps, 600);
        assert_eq!(c.get_job(id2).unwrap().fee_bps, 800);
        submit_job(&mut c, id);
        testing_env!(ctx(accounts(2)).build());
        c.approve(id, 4);
        assert_eq!(c.get_balance(agent()).0, 940_000, "settled at the locked 6%, not the new 8%");
        check(&c, 2_000_000);
    }

    #[test]
    fn auto_approve_at_deadline_not_before() {
        let mut c = setup(); gold(&mut c);
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        submit_job(&mut c, id);
        let deadline = 1_000 * NS_PER_DAY + DEFAULT_REVIEW_WINDOW_NS;
        // one nanosecond early: refused
        testing_env!(ctx(accounts(5)).block_timestamp(deadline - 1).build());
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.claim_auto_approve(id)));
        assert!(r.is_err(), "review window still open");
        assert_eq!(c.get_job(id).unwrap().status, JobStatus::Submitted);
        // at the deadline: anyone may settle it, unrated
        testing_env!(ctx(accounts(5)).block_timestamp(deadline).build());
        c.claim_auto_approve(id);
        let j = c.get_job(id).unwrap();
        assert_eq!(j.status, JobStatus::Approved);
        assert_eq!(j.rating, None);
        let p = c.get_agent(agent()).unwrap();
        assert_eq!((p.jobs_approved, p.rating_count), (1, 0));
        assert_eq!(p.rank_jobs_approved, 0, "a silent auto-approval does not move rank");
        assert_eq!(AgentHub::rank_of(&p).rank, 0);
        assert_eq!(c.get_balance(agent()).0, 940_000);
        check(&c, 1_000_000);
    }

    #[test]
    fn custom_review_window_is_honoured() {
        let mut c = setup(); gold(&mut c);
        testing_env!(ctx(token()).build());
        c.ft_on_transfer(accounts(2), U128(1_000_000), r#"{"skill":"document-extraction","min_level":1,"spec_uri":"s","review_days":1}"#.into());
        submit_job(&mut c, 0);
        testing_env!(ctx(accounts(5)).block_timestamp(1_000 * NS_PER_DAY + NS_PER_DAY).build());
        c.claim_auto_approve(0);
        assert_eq!(c.get_job(0).unwrap().status, JobStatus::Approved);
    }

    #[test]
    fn reject_opens_a_dispute_and_the_arbiter_splits_it() {
        let mut c = setup(); gold(&mut c);
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        submit_job(&mut c, id);
        testing_env!(ctx(accounts(2)).build());
        c.reject(id, "totals are wrong".into());
        let j = c.get_job(id).unwrap();
        assert_eq!(j.status, JobStatus::Disputed);
        assert_eq!(c.get_notes(id).last().unwrap().text, "Rejected: totals are wrong");
        assert_eq!(c.get_accounting().escrowed.0, 1_000_000, "nothing moved on reject");
        check(&c, 1_000_000);
        // a second reject or an approve on a disputed job is refused
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { testing_env!(ctx(accounts(2)).build()); c.reject(id, "again".into()) }));
        assert!(r.is_err());
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { testing_env!(ctx(accounts(2)).build()); c.approve(id, 5) }));
        assert!(r.is_err());
        // arbiter: 30% to the member, 70% back to the client
        testing_env!(ctx(accounts(0)).build());
        c.resolve_dispute(id, 3_000);
        let j = c.get_job(id).unwrap();
        assert_eq!(j.status, JobStatus::Resolved);
        assert_eq!(j.member_bps, Some(3_000));
        assert_eq!(j.refund, RefundStatus::Pending);
        assert_eq!(j.refund_amount.0, 700_000);
        assert_eq!(c.get_balance(agent()).0, 282_000); // 300_000 minus 6%
        assert_eq!(c.get_accounting().fees.0, 18_000);
        check(&c, 1_000_000);
        // refund lands
        c.on_refund(id, 1, Ok(()));
        assert_eq!(c.get_job(id).unwrap().refund, RefundStatus::Refunded);
        check(&c, 300_000);
        let p = c.get_agent(agent()).unwrap();
        assert_eq!((p.jobs_completed, p.jobs_approved, p.jobs_rejected), (1, 0, 1));
        // nothing can settle it twice
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { testing_env!(ctx(accounts(0)).build()); c.resolve_dispute(id, 10_000) }));
        assert!(r.is_err());
    }

    #[test]
    #[should_panic(expected = "arbiter only")]
    fn strangers_cannot_resolve_disputes() {
        let mut c = setup(); gold(&mut c);
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        submit_job(&mut c, id);
        testing_env!(ctx(accounts(2)).build());
        c.reject(id, "".into());
        testing_env!(ctx(accounts(5)).build());
        c.resolve_dispute(id, 10_000);
    }

    #[test]
    fn dispute_fallback_after_thirty_days() {
        let mut c = setup(); gold(&mut c);
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        submit_job(&mut c, id);
        testing_env!(ctx(accounts(2)).build());
        c.reject(id, "".into());
        let t = 1_000 * NS_PER_DAY + DISPUTE_FALLBACK_NS;
        testing_env!(ctx(accounts(5)).block_timestamp(t - 1).build());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.claim_dispute_fallback(id))).is_err());
        testing_env!(ctx(accounts(5)).block_timestamp(t).build());
        c.claim_dispute_fallback(id);
        let j = c.get_job(id).unwrap();
        assert_eq!((j.status, j.member_bps, j.refund_amount.0), (JobStatus::Resolved, Some(5_000), 500_000));
        assert_eq!(c.get_balance(agent()).0, 470_000);
        check(&c, 1_000_000);
    }

    #[test]
    fn failed_refund_is_owed_then_retried() {
        let mut c = setup();
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        testing_env!(ctx(accounts(2)).build());
        c.cancel(id);
        assert_eq!(c.get_job(id).unwrap().refund, RefundStatus::Pending);
        check(&c, 1_000_000);
        // client not registered on the token: ft_transfer fails
        c.on_refund(id, 1, Err(PromiseError::Failed));
        let j = c.get_job(id).unwrap();
        assert_eq!((j.status, j.refund, j.refund_amount.0), (JobStatus::Cancelled, RefundStatus::Failed, 1_000_000));
        assert_eq!(c.get_accounting().refunds_owed.0, 1_000_000);
        check(&c, 1_000_000);
        // settling or cancelling during a failed/pending refund is impossible
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { testing_env!(ctx(accounts(2)).build()); c.cancel(id) })).is_err());
        c.on_refund(id, 1, Ok(())); // late callback for the failed attempt is ignored
        assert_eq!(c.get_job(id).unwrap().refund, RefundStatus::Failed);
        assert_eq!(c.get_accounting().refunds_owed.0, 1_000_000);
        // retry succeeds
        testing_env!(ctx(accounts(2)).build());
        c.retry_refund(id);
        assert_eq!(c.get_accounting().refunds_owed.0, 0);
        check(&c, 1_000_000);
        c.on_refund(id, 2, Ok(()));
        assert_eq!(c.get_job(id).unwrap().refund, RefundStatus::Refunded);
        check(&c, 0);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { testing_env!(ctx(accounts(2)).build()); c.retry_refund(id) })).is_err(), "no double refund");
    }

    #[test]
    #[should_panic(expected = "only open (unaccepted)")]
    fn cancel_is_refused_once_accepted() {
        let mut c = setup(); gold(&mut c);
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        testing_env!(ctx(agent()).build());
        c.accept_job(id);
        testing_env!(ctx(accounts(2)).build());
        c.cancel(id);
    }

    #[test]
    fn pause_blocks_the_right_things() {
        let mut c = setup(); gold(&mut c);
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        let id2 = post_job(&mut c, accounts(2), 1_000_000, 1);
        submit_job(&mut c, id);
        c.credit(&agent(), 500_000);
        testing_env!(ctx(accounts(0)).build());
        c.set_paused(true);
        // blocked: funding, accept, withdraw, withdraw_fees
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| post_job(&mut c, accounts(2), 1, 1))).is_err());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { testing_env!(ctx(agent()).build()); c.accept_job(id2) })).is_err());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { testing_env!(ctx(agent()).build()); c.withdraw(accounts(1), U128(1)) })).is_err());
        // allowed: approve, reject/resolve, cancel (a refund)
        testing_env!(ctx(accounts(2)).build());
        c.approve(id, 5);
        c.cancel(id2);
        assert_eq!(c.get_job(id2).unwrap().refund, RefundStatus::Pending);
        check(&c, 2_500_000);
        testing_env!(ctx(accounts(0)).build());
        c.set_paused(false);
        testing_env!(ctx(agent()).build());
        c.withdraw(accounts(1), U128(1));
    }

    #[test]
    fn two_step_owner_transfer() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).build());
        c.propose_owner(accounts(4));
        assert_eq!(c.get_governance().owner, accounts(0), "nothing changes until accepted");
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { testing_env!(ctx(accounts(5)).build()); c.accept_owner() })).is_err());
        testing_env!(ctx(accounts(4)).build());
        c.accept_owner();
        let g = c.get_governance();
        assert_eq!((g.owner, g.arbiter, g.pending_owner), (accounts(4), accounts(4), None));
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { testing_env!(ctx(accounts(0)).build()); c.set_fee_bps(100) })).is_err(), "old owner is out");
    }

    #[test]
    fn upgrade_waits_for_the_delay() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).build());
        c.propose_upgrade(vec![0, 97, 115, 109]);
        testing_env!(ctx(accounts(0)).block_timestamp(1_000 * NS_PER_DAY + 59 * 1_000_000_000).build());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.apply_upgrade())).is_err(), "too early");
        assert!(c.get_governance().pending_upgrade.is_some());
        testing_env!(ctx(accounts(0)).block_timestamp(1_000 * NS_PER_DAY + 60 * 1_000_000_000).build());
        c.apply_upgrade();
        assert!(c.get_governance().pending_upgrade.is_none());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.set_upgrade_delay(1))).is_err(), "delay cannot shrink");
    }

    #[test]
    fn fee_withdrawal_round_trips_accounting() {
        let mut c = setup(); gold(&mut c);
        let id = post_job(&mut c, accounts(2), 1_000_000, 1);
        submit_job(&mut c, id);
        testing_env!(ctx(accounts(2)).build());
        c.approve(id, 5);
        testing_env!(ctx(accounts(0)).build());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.withdraw_fees(accounts(0), U128(60_001)))).is_err());
        c.withdraw_fees(accounts(0), U128(60_000));
        check(&c, 1_000_000);
        c.on_withdraw_fees(U128(60_000), Err(PromiseError::Failed));
        assert_eq!(c.get_accounting().fees.0, 60_000);
        check(&c, 1_000_000);
        c.withdraw_fees(accounts(0), U128(60_000));
        c.on_withdraw_fees(U128(60_000), Ok(()));
        check(&c, 940_000);
    }

    #[test]
    #[should_panic(expected = "owner only")]
    fn strangers_cannot_withdraw_fees() {
        let mut c = setup();
        testing_env!(ctx(accounts(5)).build());
        c.withdraw_fees(accounts(5), U128(1));
    }

    // A real EIP-191 signature produced off-chain (secp256k1 test key) over
    // "m:aa:bb" so the on-chain recovery path is exercised end to end.
    const PROOF_SIGNER: &str = "bb3c746370658caccd39f2bf50dc2d31246de4ba";
    const PROOF_SIG: &str = "dd4e9c454ebfe15bf31160292dbab89b2a647a0ff54d1354a5581fe3799cdc3b1025cd9b1564920c778fed3efe996c849231df6198814c10c89f9bf5eeab96a51c";
    const PROOF_OUT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn proof() -> ExecutionProof {
        ExecutionProof {
            model: "m".into(), chat_id: "c".into(), request_hash: "aa".into(), response_hash: "bb".into(),
            signature: PROOF_SIG.into(), signing_address: PROOF_SIGNER.into(), signature_kind: "provider_tee".into(),
        }
    }

    #[test]
    fn proof_recovers_and_requires_trust() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).build());
        let (ok, why) = c.verify_proof(proof(), 0, PROOF_OUT.into());
        assert!(!ok, "{}", why);
        c.add_trusted_signer(PROOF_SIGNER.into(), "test enclave".into());
        let (ok, why) = c.verify_proof(proof(), 0, PROOF_OUT.into());
        assert!(ok, "{}", why);
        let mut bad = proof();
        bad.response_hash = "cc".into();
        assert!(!c.verify_proof(bad, 0, PROOF_OUT.into()).0, "tampered hash must fail");
        assert!(!c.verify_proof(proof(), 1, PROOF_OUT.into()).0, "signature is bound to the job id");
        assert!(!c.verify_proof(proof(), 0, "a".repeat(64)).0, "signature is bound to the output hash");
    }

    #[test]
    #[should_panic(expected = "requires a TEE execution proof")]
    fn proof_required_jobs_reject_bare_submit() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).build());
        c.mint_cert(agent(), "document-extraction".into(), 1, "".into());
        testing_env!(ctx(token()).build());
        c.ft_on_transfer(accounts(2), U128(1_000_000), r#"{"skill":"document-extraction","min_level":1,"spec_uri":"s","require_proof":true}"#.into());
        testing_env!(ctx(agent()).build());
        c.accept_job(0);
        c.submit(0, "out".into(), None);
    }

    #[test]
    fn cancel_and_reject_refund_client() {
        let mut c = setup();
        testing_env!(ctx(accounts(0)).build());
        c.mint_cert(agent(), "document-extraction".into(), 1, "".into());

        let a = post_job(&mut c, accounts(2), 1_000_000, 1);
        testing_env!(ctx(accounts(2)).build());
        c.cancel(a);
        assert_eq!(c.get_job(a).unwrap().status, JobStatus::Cancelled);

        let b = post_job(&mut c, accounts(2), 1_000_000, 1);
        testing_env!(ctx(agent()).build());
        c.accept_job(b);
        c.submit(b, "x".into(), None);
        testing_env!(ctx(accounts(2)).build());
        c.reject(b, "wrong totals".into());
        // v6: a reject no longer refunds outright; it opens a dispute and the money stays escrowed.
        assert_eq!(c.get_job(b).unwrap().status, JobStatus::Disputed);
        assert_eq!(c.get_balance(agent()).0, 0);
        assert_eq!(c.list_jobs(Some(JobStatus::Disputed), None, None).len(), 1);
        check(&c, 2_000_000);
    }

    #[test]
    #[should_panic(expected = "budget below minimum")]
    fn tiny_budget_is_rejected() {
        let mut c = setup();
        testing_env!(ctx(token()).build());
        c.ft_on_transfer(accounts(2), U128(16), r#"{"skill":"document-extraction","min_level":1,"spec_uri":"s"}"#.into());
    }

    #[test]
    #[should_panic(expected = "spec_uri is too long")]
    fn spec_uri_is_capped() {
        let mut c = setup();
        testing_env!(ctx(token()).build());
        let spec = "s".repeat(300);
        c.ft_on_transfer(accounts(2), U128(MIN_JOB_BUDGET), format!(r#"{{"skill":"document-extraction","min_level":1,"spec_uri":"{}"}}"#, spec));
    }

    #[test]
    fn fee_rounds_up() {
        let mut c = setup(); gold(&mut c);
        let id = post_job(&mut c, accounts(2), 1_000_001, 1);
        submit_job(&mut c, id);
        testing_env!(ctx(accounts(2)).build());
        c.approve(id, 5);
        // ceil(1_000_001 * 600 / 10_000) = 60_001, so the member is paid 940_000.
        assert_eq!(c.get_balance(agent()).0, 940_000);
        assert_eq!(c.get_accounting().fees.0, 60_001);
        check(&c, 1_000_001);
    }

    #[test]
    fn self_deal_pays_but_does_not_raise_rank() {
        let mut c = setup(); gold(&mut c);
        // accounts(1) owns the agent.
        let id = post_job(&mut c, accounts(1), MIN_JOB_BUDGET, 1);
        submit_job(&mut c, id);
        testing_env!(ctx(accounts(1)).build());
        c.approve(id, 5);
        let p = c.get_agent(agent()).unwrap();
        assert_eq!(p.jobs_approved, 1, "the work record still shows the job");
        assert_eq!(p.rank_jobs_approved, 0);
        assert_eq!(AgentHub::rank_of(&p).name, "Apprentice");
        assert_eq!(c.get_balance(agent()).0, 940_000);
    }

    #[test]
    fn accept_window_returns_the_escrow() {
        let mut c = setup(); gold(&mut c);
        let id = post_job(&mut c, accounts(2), MIN_JOB_BUDGET, 1);
        testing_env!(ctx(agent()).build());
        c.accept_job(id);
        let deadline = 1_000 * NS_PER_DAY + DEFAULT_ACCEPT_WINDOW_NS;
        testing_env!(ctx(accounts(5)).block_timestamp(deadline - 1).build());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.release_expired_acceptance(id))).is_err());
        assert_eq!(c.get_job(id).unwrap().status, JobStatus::Accepted);
        testing_env!(ctx(accounts(5)).block_timestamp(deadline).build());
        c.release_expired_acceptance(id);
        let j = c.get_job(id).unwrap();
        assert_eq!(j.status, JobStatus::Expired);
        assert_eq!(j.refund, RefundStatus::Pending);
        assert_eq!(c.get_accounting().escrowed.0, 0);
        check(&c, MIN_JOB_BUDGET);
        c.on_refund(id, 1, Ok(()));
        check(&c, 0);
    }

    #[test]
    fn pending_refund_can_be_sent_again_after_a_day() {
        let mut c = setup();
        let id = post_job(&mut c, accounts(2), MIN_JOB_BUDGET, 1);
        testing_env!(ctx(accounts(2)).build());
        c.cancel(id);
        assert_eq!(c.get_accounting().in_flight.0, MIN_JOB_BUDGET);
        testing_env!(ctx(accounts(2)).block_timestamp(1_000 * NS_PER_DAY + PENDING_REFUND_TIMEOUT_NS - 1).build());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.retry_pending_refund(id))).is_err());
        testing_env!(ctx(accounts(2)).block_timestamp(1_000 * NS_PER_DAY + PENDING_REFUND_TIMEOUT_NS).build());
        c.retry_pending_refund(id);
        assert_eq!(c.get_accounting().in_flight.0, MIN_JOB_BUDGET, "the amount was already in flight");
        assert_eq!(c.get_job(id).unwrap().refund_attempt, 2);
        c.on_refund(id, 1, Ok(())); // the first attempt is stale
        assert_eq!(c.get_job(id).unwrap().refund, RefundStatus::Pending);
        c.on_refund(id, 2, Ok(()));
        assert_eq!(c.get_job(id).unwrap().refund, RefundStatus::Refunded);
        check(&c, 0);
    }

    #[test]
    fn proof_is_bound_to_the_job_and_cannot_be_replayed() {
        let mut c = setup(); gold(&mut c);
        testing_env!(ctx(accounts(0)).build());
        c.add_trusted_signer(PROOF_SIGNER.into(), "test enclave".into());
        let uri = format!("out/job-0.json#sha256={}", PROOF_OUT);
        testing_env!(ctx(token()).build());
        c.ft_on_transfer(accounts(2), U128(MIN_JOB_BUDGET), r#"{"skill":"document-extraction","min_level":1,"spec_uri":"s"}"#.into());
        testing_env!(ctx(agent()).build());
        c.accept_job(0);
        c.submit(0, uri.clone(), Some(proof()));
        assert!(c.get_job(0).unwrap().proof.is_some());
        // A second job cannot reuse the signature, even with the same file hash.
        testing_env!(ctx(token()).build());
        c.ft_on_transfer(accounts(2), U128(MIN_JOB_BUDGET), r#"{"skill":"document-extraction","min_level":1,"spec_uri":"s"}"#.into());
        testing_env!(ctx(agent()).build());
        c.accept_job(1);
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.submit(1, uri, Some(proof()))));
        assert!(err.is_err(), "replayed signature must be rejected");
        assert_eq!(c.get_job(1).unwrap().status, JobStatus::Accepted);
    }

    #[test]
    fn migrate_v6_drops_a_self_deal_from_rank_and_keeps_a_real_one() {
        use near_sdk::env;
        testing_env!(ctx(hub()).build());
        let mut old_agents = near_sdk::store::IterableMap::new(b"a".to_vec());
        old_agents.insert(agent(), migrate::AgentProfile {
            account: agent(), owner: accounts(1), model: "m".into(), skills: vec!["document-extraction".into()],
            jobs_completed: 2, jobs_approved: 2, total_earned: U128(0), withdraw_cap_per_day: U128(1),
            withdraw_allowlist: vec![accounts(1)], created_at: 0, rating_sum: 10, jobs_rejected: 0,
            rate: U128(0), display_name: "Ledger Hand".into(), rating_count: 2,
        });
        let mut old_jobs = near_sdk::store::IterableMap::new(b"j".to_vec());
        let job = |id, client, rating| migrate::Job {
            id, client, skill: "document-extraction".into(), min_level: 1, budget: U128(MIN_JOB_BUDGET),
            spec_uri: "s".into(), agent: Some(agent()), output_uri: None, status: JobStatus::Approved,
            rating: Some(rating), created_at: 1, updated_at: 2, min_rank: 0, require_proof: false, proof: None,
            fee_bps: 600, review_window_ns: DEFAULT_REVIEW_WINDOW_NS, submitted_at: Some(1), disputed_at: None,
            member_bps: Some(10_000), refund: RefundStatus::None, refund_amount: U128(0),
        };
        old_jobs.insert(0, job(0, accounts(1), 5)); // owner hired their own agent
        old_jobs.insert(1, job(1, accounts(2), 5));
        let old = migrate::State {
            owner: accounts(0), token: token(), fee_bps: 600, agents: old_agents,
            certs: near_sdk::store::LookupMap::new(b"c".to_vec()),
            jobs: old_jobs, next_job_id: 2,
            balances: near_sdk::store::LookupMap::new(b"b".to_vec()),
            withdrawn_today: near_sdk::store::LookupMap::new(b"w".to_vec()),
            trusted_signers: near_sdk::store::IterableMap::new(b"t".to_vec()),
            revocations: near_sdk::store::LookupMap::new(b"r".to_vec()),
            notes: near_sdk::store::LookupMap::new(b"n".to_vec()),
            escrowed: 0, liabilities: 0, fees: 0, in_flight: 0, refunds_owed: 0,
            pending_owner: None, arbiter: accounts(0), paused: false, upgrade_delay_ns: 1, pending_upgrade: None,
        };
        env::state_write(&old);
        drop(old); // collections flush their values to storage on drop
        let c = AgentHub::migrate();
        let p = c.get_agent(agent()).unwrap();
        assert_eq!(p.jobs_approved, 2, "public record is kept");
        assert_eq!((p.rank_jobs_approved, p.rank_rating_count), (1, 1), "only the outside client's rating counts");
        assert_eq!(c.get_job(0).unwrap().accept_window_ns, DEFAULT_ACCEPT_WINDOW_NS);
        // Running migrate again must not rebuild the world a second time.
        env::state_write(&c);
        drop(c);
        let again = AgentHub::migrate();
        assert_eq!(again.get_agent(agent()).unwrap().rank_jobs_approved, 1);
    }
}
