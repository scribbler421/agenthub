// Client posts a job: mints demo USDC if needed, then ft_transfer_call to the hub
// with the job spec. The budget is escrowed in the hub.
//   npm run post-job -- --spec samples/job-invoices.json --budget 5 --min-level 2
import fs from "node:fs";
import crypto from "node:crypto";
import { accountFromEnv, call, env, arg, usdc, fmtUsdc, ONE_YOCTO, txHash, explorerTx, view } from "./near.js";

async function main() {
  const client = await accountFromEnv("CLIENT");
  const hub = env("HUB_CONTRACT");
  const token = env("TOKEN_CONTRACT");
  const specPath = arg("spec", "samples/job-invoices.json");
  const budget = arg("budget", "5");
  const minLevel = Number(arg("min-level", "2"));

  const spec = fs.readFileSync(specPath);
  const hash = crypto.createHash("sha256").update(spec).digest("hex");
  // v1: the spec lives in the repo; the URI carries its hash so the agent can verify it.
  const specUri = `${specPath}#sha256=${hash}`;

  // Make sure the client has enough demo USDC (mock token only).
  const bal: string = await view(token, "ft_balance_of", { account_id: client.accountId });
  if (BigInt(bal) < BigInt(usdc(budget))) {
    await call(client, token, "storage_deposit", { account_id: client.accountId }, { deposit: 10_000_000_000_000_000_000_000n });
    const m = await call(client, token, "mint", { account_id: client.accountId, amount: usdc(100) });
    console.log(`minted 100 demo USDC to ${client.accountId}  ${explorerTx(txHash(m))}`);
  }

  const msg = JSON.stringify({ skill: "document-extraction", min_level: minLevel, spec_uri: specUri });
  const r = await call(client, token, "ft_transfer_call", { receiver_id: hub, amount: usdc(budget), msg }, { deposit: ONE_YOCTO, gas: 100_000_000_000_000n });
  console.log(`ft_transfer_call ${fmtUsdc(usdc(budget))} → ${hub}  ${explorerTx(txHash(r))}`);

  const jobs = await view(hub, "list_jobs", { status: "Open" });
  const job = jobs.at(-1);
  console.log(`\njob #${job.id}  status ${job.status}  budget ${fmtUsdc(job.budget)}  min level ${job.min_level}`);
  console.log(`spec ${job.spec_uri}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
