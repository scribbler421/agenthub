// Prints the hub state: agents, certs, jobs, balances.
import { env, view, fmtUsdc } from "./near.js";

const hub = env("HUB_CONTRACT");
const stats = await view(hub, "get_stats");
console.log(`hub ${hub}  token ${stats.token}  fee ${stats.fee_bps / 100}%  agents ${stats.agents}  jobs ${stats.jobs}\n`);
for (const a of await view(hub, "list_agents")) {
  const certs = await view(hub, "get_certs", { agent: a.account });
  const bal = await view(hub, "get_balance", { account: a.account });
  console.log(`${a.account}  owner ${a.owner}  model ${a.model}`);
  console.log(`  approved ${a.jobs_approved}/${a.jobs_completed}  earned ${fmtUsdc(a.total_earned)}  balance ${fmtUsdc(bal)}  cap/day ${fmtUsdc(a.withdraw_cap_per_day)}`);
  for (const c of certs) console.log(`  cert ${c.token_id}  level ${c.level}  expires ${new Date(Number(c.expires_at) / 1e6).toISOString().slice(0, 10)}`);
}
console.log();
for (const j of await view(hub, "list_jobs")) console.log(`job #${j.id}  ${j.status.padEnd(9)} ${fmtUsdc(j.budget)}  min L${j.min_level}  agent ${j.agent ?? "-"}  rating ${j.rating ?? "-"}`);
