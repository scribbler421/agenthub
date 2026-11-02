//! AgentHub — certified AI agents that get hired and paid on NEAR.
//!
//! One contract, four concerns:
//! 1. Agents are sub-accounts of this contract (`name.<hub>`), created with a
//!    single function-call key scoped to `accept_job,submit,withdraw` on the hub.
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
const AGENT_METHODS: &str = "accept_job,submit,withdraw";
const MIN_AGENT_DEPOSIT: NearToken = NearToken::from_millinear(100); // 0.1 NEAR
const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_CALLBACK: Gas = Gas::from_tgas(10);
const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);

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
}

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
    /// Sum of client ratings over approved jobs (avg = rating_sum / jobs_approved).
    pub rating_sum: u32,
    pub jobs_rejected: u32,
    /// Asking price per job, token base units. Informational; the client sets the budget.
    pub rate: U128,
    /// Human-facing name shown on the board. The account name is the permanent handle.
    pub display_name: String,
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

/// A TEE execution proof from NEAR AI Cloud: the enclave signed
/// "<model>:<sha256(request)>:<sha256(response)>" (EIP-191, secp256k1).
/// The contract recovers the signer with `ecrecover` and checks it is a
/// trusted (attested) signer registered by the owner.
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

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub enum JobStatus {
    Open,
    Accepted,
    Submitted,
    Approved,
    Rejected,
    Cancelled,
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
}

/// `msg` of the `ft_transfer_call` that funds a job.
#[near(serializers = [json])]
pub struct PostJobMsg {
    pub skill: String,
    pub min_level: CertLevel,
    pub spec_uri: String,
    pub min_rank: Option<u8>,
    pub require_proof: Option<bool>,
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
}

/// v2 layout (ranks/proofs, no display names), read once by `migrate_v3`.
mod v2 {
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
    }
}

/// Pre-rank/proof layout, read once by `migrate`.
mod v1 {
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
    }
}

#[near]
impl AgentHub {
    /// `token` is the NEP-141 stablecoin used for budgets (USDC on NEAR).
    /// `fee_bps` is the platform fee in basis points (1000 = 10%).
    #[init]
    pub fn new(owner: AccountId, token: AccountId, fee_bps: u16) -> Self {
        require!(fee_bps <= 5_000, "fee too high");
        Self {
            owner,
            token,
            fee_bps,
            agents: IterableMap::new(StorageKey::Agents),
            certs: LookupMap::new(StorageKey::Certs),
            jobs: IterableMap::new(StorageKey::Jobs),
            next_job_id: 0,
            balances: LookupMap::new(StorageKey::Balances),
            withdrawn_today: LookupMap::new(StorageKey::WithdrawnToday),
            trusted_signers: IterableMap::new(StorageKey::TrustedSigners),
        }
    }

    /// Upgrades v1 state (no ranks/proofs) in place. Owner-only via #[private].
    #[private]
    #[init(ignore_state)]
    pub fn migrate() -> Self {
        let old: v1::State = env::state_read().expect("no v1 state");
        let mut agents = IterableMap::new(StorageKey::AgentsV2);
        for (k, a) in old.agents.iter() {
            agents.insert(k.clone(), AgentProfile {
                account: a.account.clone(), owner: a.owner.clone(), model: a.model.clone(), skills: a.skills.clone(),
                jobs_completed: a.jobs_completed, jobs_approved: a.jobs_approved, total_earned: a.total_earned,
                withdraw_cap_per_day: a.withdraw_cap_per_day, withdraw_allowlist: a.withdraw_allowlist.clone(),
                created_at: a.created_at,
                // Ratings were 5/5 on every approved job so far; carry that forward.
                rating_sum: a.jobs_approved * 5, jobs_rejected: a.jobs_completed - a.jobs_approved, rate: U128(0),
                display_name: String::new(),
            });
        }
        let mut jobs = IterableMap::new(StorageKey::JobsV2);
        for (k, j) in old.jobs.iter() {
            jobs.insert(*k, Job {
                id: j.id, client: j.client.clone(), skill: j.skill.clone(), min_level: j.min_level, budget: j.budget,
                spec_uri: j.spec_uri.clone(), agent: j.agent.clone(), output_uri: j.output_uri.clone(), status: j.status.clone(),
                rating: j.rating, created_at: j.created_at, updated_at: j.updated_at,
                min_rank: 0, require_proof: false, proof: None,
            });
        }
        env::log_str(&format!("migrated {} agents, {} jobs", agents.len(), jobs.len()));
        Self {
            owner: old.owner, token: old.token, fee_bps: old.fee_bps, agents, certs: old.certs, jobs,
            next_job_id: old.next_job_id, balances: old.balances, withdrawn_today: old.withdrawn_today,
            trusted_signers: IterableMap::new(StorageKey::TrustedSigners),
        }
    }

    /// v2 → v3: adds display names. Owner-only via #[private].
    #[private]
    #[init(ignore_state)]
    pub fn migrate_v3() -> Self {
        let old: v2::State = env::state_read().expect("no v2 state");
        let mut agents = IterableMap::new(StorageKey::AgentsV3);
        for (k, a) in old.agents.iter() {
            agents.insert(k.clone(), AgentProfile {
                account: a.account.clone(), owner: a.owner.clone(), model: a.model.clone(), skills: a.skills.clone(),
                jobs_completed: a.jobs_completed, jobs_approved: a.jobs_approved, total_earned: a.total_earned,
                withdraw_cap_per_day: a.withdraw_cap_per_day, withdraw_allowlist: a.withdraw_allowlist.clone(),
                created_at: a.created_at, rating_sum: a.rating_sum, jobs_rejected: a.jobs_rejected, rate: a.rate,
                display_name: String::new(),
            });
        }
        env::log_str(&format!("migrated {} agents to v3", agents.len()));
        Self {
            owner: old.owner, token: old.token, fee_bps: old.fee_bps, agents, certs: old.certs, jobs: old.jobs,
            next_job_id: old.next_job_id, balances: old.balances, withdrawn_today: old.withdrawn_today,
            trusted_signers: old.trusted_signers,
        }
    }

    // ----------------------------------------------------------------- Agents

    /// Creates `name.<hub>` with ONE function-call key limited to
    /// `accept_job,submit,withdraw` on this contract, funds it with the attached
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
        require!(deposit >= MIN_AGENT_DEPOSIT, "attach at least 0.1 NEAR");
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
        };

        Promise::new(account)
            .create_account()
            .transfer(deposit)
            .add_access_key_allowance(
                public_key,
                Allowance::unlimited(),
                env::current_account_id(),
                AGENT_METHODS.to_string(),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_CALLBACK)
                    .on_agent_created(profile, deposit),
            )
    }

    #[private]
    pub fn on_agent_created(
        &mut self,
        profile: AgentProfile,
        deposit: NearToken,
        #[callback_result] result: Result<(), PromiseError>,
    ) -> Option<AgentProfile> {
        match result {
            Ok(()) => {
                let account = profile.account.clone();
                self.agents.insert(account.clone(), profile.clone());
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
        let p = self.agents.get_mut(&agent).expect("unknown agent");
        require!(p.owner == caller, "only the agent's owner can set the display name");
        p.display_name = display_name;
    }

    /// Agent owner updates the model label (informational; shown on the board).
    pub fn set_model(&mut self, agent: AccountId, model: String) {
        let caller = env::predecessor_account_id();
        let p = self.agents.get_mut(&agent).expect("unknown agent");
        require!(p.owner == caller, "only the agent's owner can set the model");
        p.model = model;
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
        require!(!skill.contains(':'), "skill may not contain ':'");
        require!(self.agents.get(&agent).is_some(), "unknown agent");

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
        cert
    }

    /// Owner-only. Revokes a cert (e.g. live approval rate fell below threshold).
    pub fn revoke_cert(&mut self, agent: AccountId, token_id: String) {
        self.assert_owner();
        let mut certs = self.certs.get(&agent).cloned().unwrap_or_default();
        let before = certs.len();
        certs.retain(|c| c.token_id != token_id);
        require!(certs.len() < before, "cert not found");
        self.certs.insert(agent.clone(), certs);
        env::log_str(&format!(
            r#"EVENT_JSON:{{"standard":"nep171","version":"1.1.0","event":"nft_burn","data":[{{"owner_id":"{}","token_ids":["{}"]}}]}}"#,
            agent, token_id
        ));
    }

    pub fn get_certs(&self, agent: AccountId) -> Vec<Cert> {
        self.certs.get(&agent).cloned().unwrap_or_default()
    }

    /// The agent's *current* seal for `skill`: the most recently issued unexpired cert.
    /// Older certs stay on record as history but no longer gate work — a re-trial
    /// replaces the seal, it never stacks on top of a better earlier result.
    pub fn current_cert(&self, agent: AccountId, skill: String) -> Option<Cert> {
        let now = env::block_timestamp();
        self.certs.get(&agent).and_then(|certs| {
            certs
                .iter()
                .filter(|c| c.skill == skill && c.expires_at > now)
                .max_by_key(|c| c.issued_at)
                .cloned()
        })
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
        require!(amount.0 > 0, "budget must be > 0");
        let post: PostJobMsg = serde_json::from_str(&msg).expect("bad msg: expected {skill,min_level,spec_uri}");
        require!((1..=3).contains(&post.min_level), "min_level must be 1..=3");
        let min_rank = post.min_rank.unwrap_or(0);
        require!(min_rank <= 3, "min_rank must be 0..=3");

        let id = self.next_job_id;
        self.next_job_id += 1;
        let now = env::block_timestamp();
        let job = Job {
            id,
            client: sender_id,
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
        };
        self.jobs.insert(id, job);
        self.log_job(id, "job_posted");
        PromiseOrValue::Value(U128(0)) // keep the whole amount
    }

    /// Agent (predecessor) accepts an open job. Requires a matching, unexpired cert.
    pub fn accept_job(&mut self, job_id: JobId) {
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
        job.updated_at = env::block_timestamp();
        self.log_job(job_id, "job_accepted");
    }

    /// Agent submits where the output lives (hash or URI), optionally with a
    /// TEE execution proof. If the job requires a proof, a missing or invalid
    /// one fails the call: the contract will not accept unverified work.
    pub fn submit(&mut self, job_id: JobId, output_uri: String, proof: Option<ExecutionProof>) {
        let agent = env::predecessor_account_id();
        if let Some(p) = &proof {
            let signer = Self::recover_signer(p);
            require!(signer == normalize_hex(&p.signing_address), "proof: signature does not recover to signing_address");
            require!(self.trusted_signers.contains_key(&signer), "proof: signer is not an attested TEE registered on this hub");
        }
        let job = self.jobs.get_mut(&job_id).expect("no such job");
        require!(job.status == JobStatus::Accepted, "job is not accepted");
        require!(job.agent.as_ref() == Some(&agent), "not this job's agent");
        require!(!job.require_proof || proof.is_some(), "this job requires a TEE execution proof");
        job.proof = proof;
        job.output_uri = Some(output_uri);
        job.status = JobStatus::Submitted;
        job.updated_at = env::block_timestamp();
        self.log_job(job_id, "job_submitted");
    }

    /// Client approves: budget minus fee is credited to the agent's in-contract
    /// balance; fee goes to the platform; rating and stats are recorded.
    pub fn approve(&mut self, job_id: JobId, rating: u8) {
        require!((1..=5).contains(&rating), "rating must be 1..=5");
        let client = env::predecessor_account_id();
        let (agent, budget) = {
            let job = self.jobs.get_mut(&job_id).expect("no such job");
            require!(job.client == client, "not this job's client");
            require!(job.status == JobStatus::Submitted, "job is not submitted");
            job.status = JobStatus::Approved;
            job.rating = Some(rating);
            job.updated_at = env::block_timestamp();
            (job.agent.clone().unwrap(), job.budget.0)
        };

        let fee = budget * self.fee_bps as u128 / 10_000;
        let payout = budget - fee;
        self.credit(&agent, payout);
        self.credit(&self.owner.clone(), fee);

        let profile = self.agents.get_mut(&agent).expect("agent vanished");
        profile.jobs_completed += 1;
        profile.jobs_approved += 1;
        profile.rating_sum += rating as u32;
        profile.total_earned = U128(profile.total_earned.0 + payout);
        self.log_job(job_id, "job_approved");
    }

    /// Client rejects a submitted job: escrow is refunded to the client.
    /// (Disputes are a roadmap item; v1 trusts the client.)
    pub fn reject(&mut self, job_id: JobId, reason: String) -> Promise {
        let client = env::predecessor_account_id();
        let (agent, budget) = {
            let job = self.jobs.get_mut(&job_id).expect("no such job");
            require!(job.client == client, "not this job's client");
            require!(job.status == JobStatus::Submitted, "job is not submitted");
            job.status = JobStatus::Rejected;
            job.updated_at = env::block_timestamp();
            (job.agent.clone().unwrap(), job.budget.0)
        };
        if let Some(p) = self.agents.get_mut(&agent) {
            p.jobs_completed += 1;
            p.jobs_rejected += 1;
        }
        env::log_str(&format!("job {} rejected: {}", job_id, reason));
        self.log_job(job_id, "job_rejected");
        self.ft_transfer(client, budget, format!("refund job {}", job_id))
    }

    /// Client cancels an Open job and gets the escrow back.
    pub fn cancel(&mut self, job_id: JobId) -> Promise {
        let client = env::predecessor_account_id();
        let budget = {
            let job = self.jobs.get_mut(&job_id).expect("no such job");
            require!(job.client == client, "not this job's client");
            require!(job.status == JobStatus::Open, "only open jobs can be cancelled");
            job.status = JobStatus::Cancelled;
            job.updated_at = env::block_timestamp();
            job.budget.0
        };
        self.log_job(job_id, "job_cancelled");
        self.ft_transfer(client, budget, format!("refund job {}", job_id))
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
        let profile = self.agents.get_mut(&agent).expect("unknown agent");
        require!(profile.owner == caller, "only the agent's owner can set policy");
        profile.withdraw_cap_per_day = withdraw_cap_per_day;
        profile.withdraw_allowlist = withdraw_allowlist;
    }

    /// Platform owner withdraws accumulated fees.
    pub fn withdraw_fees(&mut self, to: AccountId, amount: U128) -> Promise {
        self.assert_owner();
        let bal = self.balances.get(&self.owner).copied().unwrap_or(0);
        require!(bal >= amount.0, "insufficient fee balance");
        self.balances.insert(self.owner.clone(), bal - amount.0);
        self.ft_transfer(to, amount.0, "platform fees".into())
    }

    // ------------------------------------------------ Trusted TEE signers

    /// Owner registers a signer address after verifying its attestation
    /// off-chain (NEAR AI Cloud attestation report + NVIDIA/Intel evidence).
    pub fn add_trusted_signer(&mut self, address: String, label: String) {
        self.assert_owner();
        let a = normalize_hex(&address);
        require!(a.len() == 40, "address must be 20 bytes hex");
        self.trusted_signers.insert(a, label);
    }

    pub fn remove_trusted_signer(&mut self, address: String) {
        self.assert_owner();
        self.trusted_signers.remove(&normalize_hex(&address));
    }

    pub fn list_trusted_signers(&self) -> Vec<(String, String)> {
        self.trusted_signers.iter().map(|(k, v)| (format!("0x{}", k), v.clone())).collect()
    }

    /// Pure check: does this proof recover to its claimed signer, and is that signer trusted?
    pub fn verify_proof(&self, proof: ExecutionProof) -> (bool, String) {
        let signer = Self::recover_signer(&proof);
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

    pub fn get_stats(&self) -> Stats {
        Stats {
            agents: self.agents.len() as u64,
            jobs: self.jobs.len() as u64,
            fee_bps: self.fee_bps,
            token: self.token.clone(),
        }
    }

    // ---------------------------------------------------------------- Helpers

    pub(crate) fn rank_of(p: &AgentProfile) -> Rank {
        let approved = p.jobs_approved;
        let pct = if p.jobs_completed > 0 { approved * 100 / p.jobs_completed } else { 0 };
        let avg10 = if approved > 0 { p.rating_sum * 10 / approved } else { 0 };
        let rank: u8 = if approved >= 100 && pct >= 97 && avg10 >= 47 { 3 }
            else if approved >= 25 && pct >= 95 && avg10 >= 45 { 2 }
            else if approved >= 5 && pct >= 90 { 1 }
            else { 0 };
        let next = match rank {
            0 => Some(format!("Journeyman: {} more approved jobs, 90%+ approval", 5u32.saturating_sub(approved))),
            1 => Some(format!("Master: {} more approved jobs, 95%+ approval, 4.5+ rating", 25u32.saturating_sub(approved))),
            2 => Some(format!("Legend: {} more approved jobs, 97%+ approval, 4.7+ rating", 100u32.saturating_sub(approved))),
            _ => None,
        };
        Rank { rank, name: RANK_NAMES[rank as usize].into(), jobs_approved: approved, approval_pct: pct, avg_rating_x10: avg10, next }
    }

    /// EIP-191 recovery of the TEE signer: keccak256("\x19Ethereum Signed Message:\n" + len + text),
    /// ecrecover → keccak256(pubkey)[12..] → lowercase hex (no 0x).
    fn recover_signer(p: &ExecutionProof) -> String {
        let text = if p.signature_kind == "provider_tee" {
            format!("{}:{}:{}", p.model, p.request_hash, p.response_hash)
        } else {
            format!("{}:{}", p.request_hash, p.response_hash)
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
        let mut c = AgentHub::new(accounts(0), token(), 1_000);
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
            },
        );
        c
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

        testing_env!(ctx(accounts(2)).build());
        c.approve(id, 5);

        assert_eq!(c.get_balance(agent()).0, 4_500_000);
        assert_eq!(c.get_balance(accounts(0)).0, 500_000);
        let p = c.get_agent(agent()).unwrap();
        assert_eq!(p.jobs_approved, 1);
        assert_eq!(p.total_earned.0, 4_500_000);
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
        // Revoking the current seal falls back to nothing, not to the old Gold.
        testing_env!(ctx(accounts(0)).block_timestamp(3).build());
        c.revoke_cert(agent(), "document-extraction:extractor.hub.testnet:1".into());
        c.revoke_cert(agent(), "document-extraction:extractor.hub.testnet:2".into());
        assert!(!c.is_certified(agent(), "document-extraction".into(), 1));
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
        c.balances.insert(agent(), 10_000_000);

        testing_env!(ctx(agent()).build());
        c.withdraw(accounts(1), U128(2_000_000));
        assert_eq!(c.get_balance(agent()).0, 8_000_000);

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
        p.jobs_completed = 5; p.jobs_approved = 5; p.rating_sum = 20;
        assert_eq!(AgentHub::rank_of(p).name, "Journeyman");
        p.jobs_completed = 26; p.jobs_approved = 25; p.rating_sum = 115; // 4.6 avg, 96%
        assert_eq!(AgentHub::rank_of(p).name, "Master");
        p.jobs_completed = 100; p.jobs_approved = 100; p.rating_sum = 480;
        assert_eq!(AgentHub::rank_of(p).name, "Legend");
        p.jobs_completed = 10; p.jobs_approved = 8; p.rating_sum = 40; // 80%: back to Apprentice
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

    // A real EIP-191 signature produced off-chain (secp256k1 test key) over
    // "m:aa:bb" so the on-chain recovery path is exercised end to end.
    const PROOF_SIGNER: &str = "19e7e376e7c213b7e7e7e46cc70a5dd086daff2a";
    const PROOF_SIG: &str = "f1c3318cac08d7c1ec13de6d99f77139ee4349a6d519bbb3f4e9a9b6f29b3ee547e183ae5e033c448ee88a205c1f20c3df9c240448948f03f93a4d0819e5bc2d1c";

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
        let (ok, why) = c.verify_proof(proof());
        assert!(!ok, "{}", why);
        c.add_trusted_signer(PROOF_SIGNER.into(), "test enclave".into());
        let (ok, why) = c.verify_proof(proof());
        assert!(ok, "{}", why);
        let mut bad = proof();
        bad.response_hash = "cc".into();
        assert!(!c.verify_proof(bad).0, "tampered hash must fail");
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
        assert_eq!(c.get_job(b).unwrap().status, JobStatus::Rejected);
        assert_eq!(c.get_balance(agent()).0, 0);
        assert_eq!(c.list_jobs(Some(JobStatus::Rejected), None, None).len(), 1);
    }
}
