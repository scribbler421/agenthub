// The escrow security demo (v6). Runs against testnet with real accounts:
//   client/owner = OPERATOR (nexus0.testnet), member = --name (default herald),
//   stranger = the mock token's account (we hold its key; it is neither client, owner nor member).
// Every attack must fail WITHOUT changing the hub's accounting or token balance; the
// legitimate flows (approve, dispute split, failed refund + retry, auto-approve guard) must work.
//   npm run attack:escrow -- --name herald
import fs from "node:fs";
import crypto from "node:crypto";
import { accountFromEnv, agentAccount, call, env, arg, view, usdc, fmtUsdc, txHash, explorerTx, NEAR } from "./near.js";

type Row = { attack: string; expected: string; result: string; ok: boolean };
const rows: Row[] = [];
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function reason(e: any): string {
  const exec = e?.kind?.kind?.FunctionCallError?.ExecutionError ?? e?.kind?.FunctionCallError?.ExecutionError;
  if (exec) return String(exec).replace(/^Smart contract panicked: /, "contract: ").slice(0, 110);
  const t = e?.type ?? "";
  if (t === "MethodNameMismatch") return "protocol: method not in the key's allowed list";
  if (t === "ReceiverMismatch") return "protocol: key is only valid for the hub contract";
  return (t ? t + ": " : "") + String(e?.message ?? e).split("\n")[0].slice(0, 100);
}

async function main() {
  const hub = env("HUB_CONTRACT"), token = env("TOKEN_CONTRACT");
  const client = await accountFromEnv("OPERATOR"); // also owner + arbiter on testnet
  const memberId = `${arg("name", "herald")}.${hub}`;
  const member = await agentAccount(memberId);
  const stranger = await agentAccount(token); // the token's own account, used as "someone else"
  const strangerId = stranger.accountId;

  const snapshot = async () => ({ acct: await view(hub, "get_accounting"), usdc: String(await view(token, "ft_balance_of", { account_id: hub })) });
  const same = (a: any, b: any) => JSON.stringify(a) === JSON.stringify(b);

  /** An attack must throw AND leave accounting + token balance untouched. */
  async function mustFail(attack: string, expected: string, fn: () => Promise<any>) {
    const before = await snapshot();
    let threw = false, why = "";
    try { await fn(); } catch (e) { threw = true; why = reason(e); }
    const after = await snapshot();
    const untouched = same(before, after);
    rows.push({ attack, expected, result: threw ? `blocked: ${why}${untouched ? "" : "  BUT BALANCES MOVED"}` : "SUCCEEDED (bad)", ok: threw && untouched });
  }
  async function mustWork(step: string, expected: string, fn: () => Promise<any>) {
    try { const r = await fn(); rows.push({ attack: step, expected, result: typeof r === "string" ? r : "ok", ok: true }); }
    catch (e) { rows.push({ attack: step, expected, result: `FAILED: ${reason(e)}`, ok: false }); }
  }
  const postJob = async (from: any, budget: string, extra: Record<string, unknown> = {}) => {
    const specPath = "samples/job-invoices.json";
    const hash = crypto.createHash("sha256").update(fs.readFileSync(specPath)).digest("hex");
    const msg = JSON.stringify({ skill: "document-extraction", min_level: 1, spec_uri: `${specPath}#sha256=${hash}`, ...extra });
    await call(from, hub, "storage_deposit", {}, { deposit: NEAR / 20n });
    await call(from, token, "ft_transfer_call", { receiver_id: hub, amount: budget, msg }, { deposit: 1n, gas: 100_000_000_000_000n });
    const jobs: any[] = await view(hub, "list_jobs", { status: "Open" });
    return jobs.filter((j) => j.client === from.accountId).at(-1).id as number;
  };
  const doWork = async (jobId: number) => {
    await call(member, hub, "accept_job", { job_id: jobId });
    await call(member, hub, "submit", { job_id: jobId, output_uri: "out/attack-demo.json#sha256=0", proof: null });
  };
  const job = (id: number) => view(hub, "get_job", { job_id: id });

  console.log(`hub ${hub}  client/owner ${client.accountId}  member ${memberId}  stranger ${strangerId}`);
  console.log("accounting before:", JSON.stringify(await snapshot()), "\n");

  // ---------------------------------------------------------------- 1. live job for the stranger attacks
  const j1 = await postJob(client, usdc(1));
  await doWork(j1);

  await mustFail("Fake token calls ft_on_transfer", "contract: unsupported token",
    () => call(client, hub, "ft_on_transfer", { sender_id: client.accountId, amount: usdc(100), msg: "{}" }));
  await mustFail("Stranger approves someone else's job", "contract: not this job's client", () => call(stranger, hub, "approve", { job_id: j1, rating: 5 }));
  await mustFail("Stranger rejects it", "contract: not this job's client", () => call(stranger, hub, "reject", { job_id: j1, reason: "x" }));
  await mustFail("Stranger cancels it", "contract: not this job's client", () => call(stranger, hub, "cancel", { job_id: j1 }));
  await mustFail("Stranger resolves a dispute", "contract: arbiter only", () => call(stranger, hub, "resolve_dispute", { job_id: j1, member_bps: 10000 }));
  await mustFail("Stranger withdraws guild fees", "contract: owner only", () => call(stranger, hub, "withdraw_fees", { to: strangerId, amount: "1" }));
  await mustFail("Stranger pauses the hub", "contract: owner only", () => call(stranger, hub, "set_paused", { paused: true }));
  await mustFail("Stranger claims auto-approve inside the review window", "contract: review window still open", () => call(stranger, hub, "claim_auto_approve", { job_id: j1 }));

  // ---------------------------------------------------------------- 2. member policy
  const profile = await view(hub, "get_agent", { account: memberId });
  await mustFail("Member raises its own cap / allowlist", "key: method not allowed",
    () => call(member, hub, "set_policy", { agent: memberId, withdraw_cap_per_day: "340282366920938463463374607431768211455", withdraw_allowlist: [strangerId] }));
  await mustFail("Member withdraws to a non-allowlisted address", "contract: recipient not in allowlist", () => call(member, hub, "withdraw", { to: strangerId, amount: "1" }));
  await mustFail("Member withdraws over the daily cap", "contract: exceeds daily withdrawal cap",
    () => call(member, hub, "withdraw", { to: profile.withdraw_allowlist[0], amount: (BigInt(profile.withdraw_cap_per_day) + 1n).toString() }));

  // ---------------------------------------------------------------- 3. fee
  await mustFail("Owner raises the fee above the 8% cap", "contract: fee too high", () => call(client, hub, "set_fee_bps", { fee_bps: 801 }));
  await mustWork("Owner raises the fee to 8%; existing job keeps 6%", "job fee_bps stays 600", async () => {
    await call(client, hub, "set_fee_bps", { fee_bps: 800 });
    const fb = (await job(j1)).fee_bps;
    await call(client, hub, "set_fee_bps", { fee_bps: 600 });
    if (fb !== 600) throw new Error(`job fee changed to ${fb}`);
    return `job #${j1} fee_bps=${fb} while hub fee was 800`;
  });

  // ---------------------------------------------------------------- 4. upgrade delay
  await mustWork("Owner proposes an upgrade", "staged, not applied", async () => { await call(client, hub, "propose_upgrade", { code: [0, 97, 115, 109] }); return "staged"; });
  await mustFail("Owner applies the upgrade before the delay", "contract: upgrade delay has not passed", () => call(client, hub, "apply_upgrade", {}));
  await call(client, hub, "cancel_upgrade", {});

  // ---------------------------------------------------------------- 5. approve, then double-settle attempts
  await mustWork(`Client approves job #${j1}`, "member paid minus 6%", async () => { await call(client, hub, "approve", { job_id: j1, rating: 5 }); return `member_bps=${(await job(j1)).member_bps}`; });
  await mustFail("Client approves it again", "contract: job is not submitted", () => call(client, hub, "approve", { job_id: j1, rating: 5 }));
  await mustFail("Client rejects after approving", "contract: job is not submitted", () => call(client, hub, "reject", { job_id: j1, reason: "changed my mind" }));

  // ---------------------------------------------------------------- 6. dispute: reject → split → refund
  const j2 = await postJob(client, usdc(1));
  await doWork(j2);
  await mustWork(`Client rejects job #${j2}`, "Disputed; nothing moves", async () => { await call(client, hub, "reject", { job_id: j2, reason: "two totals wrong" }); return (await job(j2)).status; });
  await mustFail("Client approves a disputed job", "contract: job is not submitted", () => call(client, hub, "approve", { job_id: j2, rating: 1 }));
  await mustFail("Client rejects it a second time", "contract: job is not submitted", () => call(client, hub, "reject", { job_id: j2, reason: "again" }));
  await mustFail("Anyone claims the 30-day fallback early", "contract: dispute fallback not yet available", () => call(stranger, hub, "claim_dispute_fallback", { job_id: j2 }));
  await mustWork(`Arbiter splits #${j2} 40/60`, "member 0.376, client refunded 0.60", async () => {
    const r = await call(client, hub, "resolve_dispute", { job_id: j2, member_bps: 4000 }, { gas: 100_000_000_000_000n });
    await sleep(1500);
    const j = await job(j2);
    return `status ${j.status} refund ${j.refund} ${explorerTx(txHash(r))}`;
  });
  await mustFail("Arbiter resolves it again", "contract: job is not disputed", () => call(client, hub, "resolve_dispute", { job_id: j2, member_bps: 10000 }));

  // ---------------------------------------------------------------- 7. refund to a client no longer registered on the token
  await call(stranger, token, "storage_deposit", { account_id: strangerId }, { deposit: 1_250_000_000_000_000_000_000n });
  await call(stranger, token, "mint", { account_id: strangerId, amount: usdc(1) });
  const j3 = await postJob(stranger, usdc(1));
  await call(stranger, token, "storage_unregister", { force: true }, { deposit: 1n });
  await mustWork(`Unregistered client cancels #${j3}`, "RefundFailed, amount owed", async () => {
    await call(stranger, hub, "cancel", { job_id: j3 }, { gas: 100_000_000_000_000n });
    await sleep(1500);
    const j = await job(j3), a = await view(hub, "get_accounting");
    if (j.refund !== "Failed") throw new Error(`refund state ${j.refund}`);
    return `refund=${j.refund} owed=${fmtUsdc(j.refund_amount)} accounting.refunds_owed=${fmtUsdc(a.refunds_owed)}`;
  });
  await mustFail("Cancel again while the refund is owed", "contract: only open (unaccepted) jobs", () => call(stranger, hub, "cancel", { job_id: j3 }));
  await mustFail("Stranger (not client/owner) retries the refund", "contract: client or owner only", () => call(member, hub, "retry_refund", { job_id: j3 }));
  await mustWork(`Client re-registers and retries #${j3}`, "Refunded", async () => {
    await call(stranger, token, "storage_deposit", { account_id: strangerId }, { deposit: 1_250_000_000_000_000_000_000n });
    await call(stranger, hub, "retry_refund", { job_id: j3 }, { gas: 100_000_000_000_000n });
    await sleep(1500);
    const j = await job(j3);
    if (j.refund !== "Refunded") throw new Error(`refund state ${j.refund}`);
    return `refund=${j.refund} client USDC=${fmtUsdc(await view(token, "ft_balance_of", { account_id: strangerId }))}`;
  });
  await mustFail("Retry an already-sent refund", "contract: no failed refund on this job", () => call(stranger, hub, "retry_refund", { job_id: j3 }));

  // ---------------------------------------------------------------- 8. accounting reconciles
  const end = await snapshot();
  rows.push({ attack: "Accounting total equals the hub's USDC balance", expected: "equal", result: `${fmtUsdc(end.acct.total)} vs ${fmtUsdc(end.usdc)}`, ok: end.acct.total === end.usdc });

  console.log("STEP".padEnd(62) + "EXPECTED".padEnd(46) + "RESULT");
  for (const r of rows) console.log(`${r.ok ? "✔" : "✘"} ${r.attack.padEnd(60)} ${r.expected.padEnd(45)} ${r.result}`);
  const bad = rows.filter((r) => !r.ok).length;
  console.log(bad ? `\n${bad} unexpected outcome(s)` : "\nEvery attack contained; every refund accounted for.");
  process.exit(bad ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(1); });
