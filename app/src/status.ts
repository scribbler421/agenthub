// Prints the hub state (read-only; for a pass/fail check of the README claims use `npm run verify`): agents, certs, jobs, balances.
import { env, view, fmtUsdc } from "./near.js";

const hub = env("HUB_CONTRACT");
const stats = await view(hub, "get_stats");
console.log(`hub ${hub}  token ${stats.token}  fee ${stats.fee_bps / 100}%  agents ${stats.agents}  jobs ${stats.jobs}\n`);
for (const a of await view(hub, "list_agents")) {
  const certs = await view(hub, "get_certs", { agent: a.account });
  const bal = await view(hub, "get_balance", { account: a.account });
  console.log(`${a.display_name || a.account}  (${a.account})  owner ${a.owner}  model ${a.model}`);
  const rank = await view(hub, "get_rank", { agent: a.account });
  console.log(`  rank ${rank.name}  approved ${a.jobs_approved}/${a.jobs_completed}  rating ${(rank.avg_rating_x10 / 10).toFixed(1)}  earned ${fmtUsdc(a.total_earned)}  balance ${fmtUsdc(bal)}  rate ${fmtUsdc(a.rate)}  cap/day ${fmtUsdc(a.withdraw_cap_per_day)}`);
  if (rank.next) console.log(`  next: ${rank.next}`);
  for (const c of certs) console.log(`  cert ${c.token_id}  level ${c.level}  expires ${new Date(Number(c.expires_at) / 1e6).toISOString().slice(0, 10)}`);
}
console.log();
for (const j of await view(hub, "list_jobs")) console.log(`job #${j.id}  ${j.status.padEnd(9)} ${fmtUsdc(j.budget)}  min L${j.min_level} R${j.min_rank}${j.require_proof ? " proof" : ""}  agent ${j.agent ?? "-"}  rating ${j.rating ?? "-"}${j.proof ? `  TEE ${j.proof.signing_address.slice(0, 10)}…` : ""}`);
const signers = await view(hub, "list_trusted_signers");
if (signers.length) console.log(`\ntrusted TEE signers: ${signers.map((s: any) => `${s[0]} (${s[1]})`).join(", ")}`);
