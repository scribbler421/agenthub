// Client reviews the output and approves (pays from escrow) or rejects (refund).
//   npm run approve -- --job 0 --rating 5
//   npm run approve -- --job 0 --reject "wrong totals"
import fs from "node:fs";
import { accountFromEnv, call, env, arg, hasFlag, view, txHash, explorerTx, fmtUsdc } from "./near.js";

async function main() {
  const client = await accountFromEnv("CLIENT");
  const hub = env("HUB_CONTRACT");
  const jobId = Number(arg("job"));
  const job = await view(hub, "get_job", { job_id: jobId });
  if (!job) throw new Error("no such job");
  console.log(`job #${jobId}  status ${job.status}  agent ${job.agent}  output ${job.output_uri}`);

  const [outPath] = String(job.output_uri ?? "").split("#");
  if (outPath && fs.existsSync(outPath)) {
    const out = JSON.parse(fs.readFileSync(outPath, "utf8"));
    for (const r of out.results) console.log(`  ${r.id}: ${r.fields.vendor.value} | ${r.fields.invoice_number.value} | ${r.fields.invoice_date.value} | ${r.fields.total.value} ${r.fields.currency.value}`);
  }

  const before = await view(hub, "get_balance", { account: job.agent });
  if (hasFlag("reject")) {
    const r = await call(client, hub, "reject", { job_id: jobId, reason: arg("reject") });
    console.log(`rejected; escrow refunded  ${explorerTx(txHash(r))}`);
    return;
  }
  const rating = Number(arg("rating", "5"));
  const r = await call(client, hub, "approve", { job_id: jobId, rating });
  console.log(`approved with rating ${rating}  ${explorerTx(txHash(r))}`);
  const after = await view(hub, "get_balance", { account: job.agent });
  const profile = await view(hub, "get_agent", { account: job.agent });
  console.log(`agent balance ${fmtUsdc(before)} → ${fmtUsdc(after)}   jobs approved ${profile.jobs_approved}/${profile.jobs_completed}   total earned ${fmtUsdc(profile.total_earned)}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
