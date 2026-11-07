//! v6 state, the layout live on `hub.nexus0.testnet` before this upgrade.
//! Field order is the borsh layout. Do not reorder it.

use near_sdk::json_types::U128;
use near_sdk::store::{IterableMap, LookupMap};
use near_sdk::{near, AccountId};

use crate::{Cert, CertLevel, ExecutionProof, JobId, JobStatus, Note, RefundStatus};

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
    pub rating_count: u32,
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
    pub fee_bps: u16,
    pub review_window_ns: u64,
    pub submitted_at: Option<u64>,
    pub disputed_at: Option<u64>,
    pub member_bps: Option<u16>,
    pub refund: RefundStatus,
    pub refund_amount: U128,
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
    pub revocations: LookupMap<AccountId, Vec<crate::Revocation>>,
    pub notes: LookupMap<JobId, Vec<Note>>,
    pub escrowed: u128,
    pub liabilities: u128,
    pub fees: u128,
    pub in_flight: u128,
    pub refunds_owed: u128,
    pub pending_owner: Option<AccountId>,
    pub arbiter: AccountId,
    pub paused: bool,
    pub upgrade_delay_ns: u64,
    pub pending_upgrade: Option<([u8; 32], u64)>,
}
