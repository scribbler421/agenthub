// Disputes, deadlines and refunds (hub v6).
//   npm run dispute -- --job 15 --resolve 4000       arbiter: 40% to the member, 60% back to the client
//   npm run dispute -- --job 15 --fallback           anyone, 30 days after the reject: 50/50
//   npm run dispute -- --job 12 --auto-approve       anyone, after the review window: settles as approved
//   npm run dispute -- --job 16 --retry-refund       client or owner: re-send a failed refund
//   npm run dispute -- --job 17 --release            client (hub v7): member accepted 3+ days ago and never submitted; back to Open
//   npm run dispute -- --job 15                      show the job's settlement state
import { accountFromEnv, call, env, arg, hasFlag, view, txHash, explorerTx, fmtUsdc } from "./near.js";

async function main() {
  const hub = env("HUB_CONTRACT");
  const jobId = Number(arg("job"));
  const me = await accountFromEnv("OPERATOR"); // owner = arbiter on testnet; also the client here
  const gas = 100_000_000_000_000n;
  let r: any = null;
  if (hasFlag("resolve")) r = await call(me, hub, "resolve_dispute", { job_id: jobId, member_bps: Number(arg("resolve")) }, { gas });
  else if (hasFlag("fallback")) r = await call(me, hub, "claim_dispute_fallback", { job_id: jobId }, { gas });
  else if (hasFlag("auto-approve")) r = await call(me, hub, "claim_auto_approve", { job_id: jobId }, { gas });
  else if (hasFlag("retry-refund")) r = await call(me, hub, "retry_refund", { job_id: jobId }, { gas });
  else if (hasFlag("release")) r = await call(me, hub, "release_abandoned", { job_id: jobId }, { gas }); // v7: client, 3 days after an accept with no submit
  if (r) console.log(`done  ${explorerTx(txHash(r))}`);
  await new Promise((d) => setTimeout(d, 1200));
  const j = await view(hub, "get_job", { job_id: jobId });
  const ns = (n: string | number | null) => (n ? new Date(Number(n) / 1e6).toISOString().slice(0, 16) : "–");
  console.log(`job #${j.id}  ${j.status}  budget ${fmtUsdc(j.budget)}  fee ${j.fee_bps / 100}%  member_bps ${j.member_bps ?? "–"}`);
  console.log(`  submitted ${ns(j.submitted_at)}  review window ${Number(j.review_window_ns) / 864e11} d  → auto-approve after ${j.submitted_at ? ns(Number(j.submitted_at) + Number(j.review_window_ns)) : "–"}`);
  console.log(`  disputed ${ns(j.disputed_at)}  refund ${j.refund} ${j.refund !== "None" ? fmtUsdc(j.refund_amount) : ""}`);
  console.log(`  accounting ${JSON.stringify(await view(hub, "get_accounting"))}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
