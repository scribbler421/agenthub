// The security demo. Pretend the agent has been fully compromised (prompt
// injection, stolen key, whatever) and try to do damage with ITS key.
//   npm run attack -- --name extractor
// Expected: everything outside the policy fails; one small allowed withdrawal succeeds.
import { agentAccount, call, env, arg, view, usdc, fmtUsdc } from "./near.js";

type Row = { attack: string; expected: string; result: string; ok: boolean };

async function attempt(rows: Row[], attack: string, expected: string, fn: () => Promise<any>, shouldFail: boolean) {
  try {
    await fn();
    rows.push({ attack, expected, result: shouldFail ? "SUCCEEDED (bad)" : "succeeded", ok: !shouldFail });
  } catch (e: any) {
    rows.push({ attack, expected, result: `blocked: ${reason(e)}`, ok: shouldFail });
  }
}

/** Turn a near-api-js error into the one line a human wants to read. */
function reason(e: any): string {
  const exec = e?.kind?.kind?.FunctionCallError?.ExecutionError ?? e?.kind?.FunctionCallError?.ExecutionError;
  if (exec) return String(exec).replace(/^Smart contract panicked: /, "contract: ");
  const t = e?.type ?? "";
  if (t === "RequiresFullAccess") return "protocol: this key is not a full-access key";
  if (t === "ReceiverMismatch") return "protocol: key is only valid for the hub contract";
  if (t === "MethodNameMismatch") return "protocol: method not in the key's allowed list";
  if (t === "DepositWithFunctionCall") return "protocol: function-call keys cannot attach deposits";
  if (t === "AccessKeyNotFound") return "protocol: no such key on the account";
  return (t ? t + ": " : "") + String(e?.message ?? e).split("\n")[0].slice(0, 100);
}

async function main() {
  const hub = env("HUB_CONTRACT");
  const token = env("TOKEN_CONTRACT");
  const agentId = `${arg("name")}.${hub}`;
  const agent = await agentAccount(agentId);
  const profile = await view(hub, "get_agent", { account: agentId });
  const bal = await view(hub, "get_balance", { account: agentId });
  const attacker = "attacker.testnet";
  console.log(`agent ${agentId}  balance ${fmtUsdc(bal)}  cap/day ${fmtUsdc(profile.withdraw_cap_per_day)}  allowlist ${profile.withdraw_allowlist.join(",")}\n`);

  const rows: Row[] = [];

  await attempt(rows, "Raise own daily cap to unlimited (set_policy)", "key rejected: method not allowed",
    () => call(agent, hub, "set_policy", { agent: agentId, withdraw_cap_per_day: "340282366920938463463374607431768211455", withdraw_allowlist: [attacker] }), true);

  await attempt(rows, "Mint itself a Gold cert (mint_cert)", "key rejected: method not allowed",
    () => call(agent, hub, "mint_cert", { agent: agentId, skill: "document-extraction", level: 3, score_uri: "forged" }), true);

  await attempt(rows, "Call the token contract directly (ft_transfer)", "key rejected: wrong receiver",
    () => call(agent, token, "ft_transfer", { receiver_id: attacker, amount: "1" }), true);

  await attempt(rows, "Send NEAR out of the agent account", "no full-access key exists",
    () => agent.sendMoney(attacker, 1n), true);

  await attempt(rows, "Add a new full-access key to the agent account", "no full-access key exists",
    () => agent.addKey("ed25519:3tysLvy7KGoE8pznUgXvSHa4vYyGvrDZFcT8jgb8PEU6"), true);

  await attempt(rows, "withdraw to an address NOT in the allowlist", "contract: recipient not in allowlist",
    () => call(agent, hub, "withdraw", { to: attacker, amount: "1" }), true);

  const over = (BigInt(profile.withdraw_cap_per_day) + 1n).toString();
  await attempt(rows, `withdraw ${fmtUsdc(over)} (over the daily cap) to allowlisted`, "contract: exceeds daily withdrawal cap",
    () => call(agent, hub, "withdraw", { to: profile.withdraw_allowlist[0], amount: over }), true);

  const small = BigInt(bal) > 0n ? (BigInt(bal) < BigInt(usdc(1)) ? bal : usdc(1)) : "0";
  if (small !== "0") {
    await attempt(rows, `withdraw ${fmtUsdc(small)} to allowlisted owner (legit)`, "succeeds",
      () => call(agent, hub, "withdraw", { to: profile.withdraw_allowlist[0], amount: small }), false);
  }

  console.log("ATTACK".padEnd(58) + "EXPECTED".padEnd(40) + "RESULT");
  for (const r of rows) console.log(`${r.ok ? "✔" : "✘"} ${r.attack.padEnd(56)} ${r.expected.padEnd(39)} ${r.result}`);
  const bad = rows.filter((r) => !r.ok).length;
  console.log(bad ? `\n${bad} unexpected outcome(s)` : "\nAll attacks contained. The agent key can do its job and nothing else.");
  process.exit(bad ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(1); });
