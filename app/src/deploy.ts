// Deploys mock USDC and the hub as sub-accounts of the operator, initialises
// both, and registers the hub on the token for storage.
//   npm run deploy                 (uses ../out/*.wasm from CI or `cargo near build`)
//   npm run deploy -- --fee-bps 1000 --redeploy
import fs from "node:fs";
import path from "node:path";
import { KeyPair, type Account } from "near-api-js";
import { accountFromEnv, agentAccount, call, NEAR, arg, hasFlag, txHash, explorerTx, near, KEYS_DIR } from "./near.js";

const WASM_DIR = path.resolve("../out");

async function accountExists(id: string): Promise<boolean> {
  try {
    await (await near()).connection.provider.query({ request_type: "view_account", finality: "final", account_id: id });
    return true;
  } catch {
    return false;
  }
}

async function deployOne(operator: Account, sub: string, wasmFile: string, initArgs: object, fund: bigint): Promise<string> {
  const id = `${sub}.${operator.accountId}`;
  const wasm = new Uint8Array(fs.readFileSync(path.join(WASM_DIR, wasmFile)));
  const existed = await accountExists(id);

  if (!existed) {
    const key = KeyPair.fromRandom("ed25519");
    const r = await operator.createAccount(id, key.getPublicKey(), fund);
    console.log(`created ${id}  ${explorerTx(txHash(r))}`);
    fs.mkdirSync(KEYS_DIR, { recursive: true });
    fs.writeFileSync(path.join(KEYS_DIR, `${id}.json`), JSON.stringify({ accountId: id, privateKey: key.toString() }, null, 2));
  } else if (!hasFlag("redeploy")) {
    console.log(`${id} already exists; pass --redeploy to overwrite its code`);
    return id;
  }

  const acc = await agentAccount(id); // reuses the saved sub-account key
  const r = await acc.deployContract(wasm);
  console.log(`deployed ${wasmFile} → ${id}  ${explorerTx(txHash(r))}`);
  if (!existed) {
    const i = await call(acc, id, "new", initArgs);
    console.log(`initialised ${id}  ${explorerTx(txHash(i))}`);
  }
  return id;
}

async function main() {
  const operator = await accountFromEnv("OPERATOR");
  const feeBps = Number(arg("fee-bps", "1000"));

  const token = await deployOne(operator, "usdc", "mock_usdc.wasm", {}, 3n * NEAR);
  const hub = await deployOne(operator, "hub", "agenthub.wasm", { owner: operator.accountId, token, fee_bps: feeBps }, 5n * NEAR);

  // Both the hub and the operator must be registered on the token to hold it.
  for (const account_id of [hub, operator.accountId]) {
    const r = await call(operator, token, "storage_deposit", { account_id }, { deposit: NEAR / 100n });
    console.log(`storage registered for ${account_id}  ${explorerTx(txHash(r))}`);
  }

  console.log("\nAdd to .env:");
  console.log(`HUB_CONTRACT=${hub}`);
  console.log(`TOKEN_CONTRACT=${token}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
