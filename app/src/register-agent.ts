// Registers an agent: the hub creates `<name>.<hub>` with ONE function-call key
// (accept_job, submit, withdraw, post_note) and no full-access key. The key is saved to
// .keys/<agent>.json for the runner. The caller (operator) becomes its owner.
//   npm run register-agent -- --name extractor --cap 50 --allow yourname.testnet
import fs from "node:fs";
import path from "node:path";
import { KeyPair } from "near-api-js";
import { accountFromEnv, call, env, arg, usdc, NEAR, KEYS_DIR, txHash, explorerTx, view } from "./near.js";

async function main() {
  const operator = await accountFromEnv("OPERATOR");
  const hub = env("HUB_CONTRACT");
  const name = arg("name");
  const cap = arg("cap", "50"); // USDC per day
  const allow = arg("allow", operator.accountId).split(",");
  const model = arg("model", process.env.LLM_PROVIDER === "nearai" ? (process.env.NEARAI_MODEL ?? "Qwen/Qwen3.8-27B") : (process.env.LLM_MODEL ?? "claude-haiku-5-5"));
  const rate = arg("rate", "3");
  const displayName = arg("display-name", name);
  const agentId = `${name}.${hub}`;

  const key = KeyPair.fromRandom("ed25519");
  const r = await call(
    operator,
    hub,
    "register_agent",
    {
      name,
      public_key: key.getPublicKey().toString(),
      model,
      skills: ["document-extraction"],
      withdraw_cap_per_day: usdc(cap),
      withdraw_allowlist: allow,
      rate: usdc(rate),
      display_name: displayName,
    },
    { deposit: (NEAR * BigInt(Math.round(Number(arg("fund", "0.5")) * 100))) / 100n, gas: 150_000_000_000_000n }, // --fund <NEAR> seeds the member's gas
  );
  console.log(`register_agent tx  ${explorerTx(txHash(r))}`);

  const profile = await view(hub, "get_agent", { account: agentId });
  if (!profile) throw new Error("agent not registered — check the transaction above");

  fs.mkdirSync(KEYS_DIR, { recursive: true });
  fs.writeFileSync(path.join(KEYS_DIR, `${agentId}.json`), JSON.stringify({ accountId: agentId, privateKey: key.toString() }, null, 2));

  // Register the agent on the token so it could receive tokens if ever needed.
  const token = env("TOKEN_CONTRACT");
  await call(operator, token, "storage_deposit", { account_id: agentId }, { deposit: NEAR / 100n });

  console.log(`\nagent ${agentId}`);
  console.log(`  owner      ${profile.owner}`);
  console.log(`  cap/day    ${cap} USDC`);
  console.log(`  allowlist  ${allow.join(", ")}`);
  console.log(`  key        function-call only → ${hub} [accept_job, submit, withdraw, post_note]`);
  console.log(`  saved      .keys/${agentId}.json`);
}

main().catch((e) => { console.error(e); process.exit(1); });
