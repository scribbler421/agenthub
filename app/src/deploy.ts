// Deploys mock USDC and the hub as sub-accounts of the operator, initialises
// both, and registers the hub on the token for storage.
//   npm run deploy                      (uses ../out/*.wasm from CI or cargo near build)
//   npm run deploy -- --fee-bps 1000
import fs from "node:fs";
import path from "node:path";
import { KeyPair } from "near-api-js";
import { accountFromEnv, call, NEAR, arg, hasFlag, txHash, explorerTx } from "./near.js";

const WASM_DIR = path.resolve("../out");

async function deployOne(operatorId: string, operator: any, sub: string, wasmFile: string, initMethod: string, initArgs: object) {
  const id = `${sub}.${operatorId}`;
  const wasm = fs.readFileSync(path.join(WASM_DIR, wasmFile));
  const pk = (await (operator as any).connection.signer.getPublicKey(operatorId, "testnet")) ?? null;

  let exists = false;
  try { await (await (operator as any).connection.provider.query({ request_type: "view_account", finality: "final", account_id: id })); exists = true; } catch {}

  if (!exists) {
    const key = KeyPair.fromRandom("ed25519");
    await (operator as any).connection.signer.keyStore?.setKey?.("testnet", id, key);
    const r = await operator.createAccount(id, key.getPublicKey(), 3n * NEAR);
    console.log(`created ${id}  ${explorerTx(txHash(r))}`);
    // keep the sub-account key so we can redeploy later
    fs.mkdirSync(".keys", { recursive: true });
    fs.writeFileSync(`.keys/${id}.json`, JSON.stringify({ accountId: id, privateKey: key.toString() }, null, 2));
  } else if (!hasFlag("redeploy")) {
    console.log(`${id} exists; pass --redeploy to overwrite code`);
    return id;
  }

  const { agentAccount } = await import("./near.js");
  const acc = await agentAccount(id);
  const r = await acc.deployContract(new Uint8Array(wasm));
  console.log(`deployed ${wasmFile} → ${id}  ${explorerTx(txHash(r))}`);
  if (!exists) {
    const i = await call(acc, id, initMethod, initArgs);
    console.log(`initialised ${id}  ${explorerTx(txHash(i))}`);
  }
  void pk;
  return id;
}

async function main() {
  const operator = await accountFromEnv("OPERATOR");
  const operatorId = operator.accountId;
  const feeBps = Number(arg("fee-bps", "1000"));

  const token = await deployOne(operatorId, operator, "usdc", "mock_usdc.wasm", "new", {});
  const hub = await deployOne(operatorId, operator, "hub", "agenthub.wasm", "new", {
    owner: operatorId,
    token,
    fee_bps: feeBps,
  });

  // The hub must be registered on the token to receive and send it.
  const r = await call(operator, token, "storage_deposit", { account_id: hub }, { deposit: NEAR / 100n });
  console.log(`storage registered for hub on token  ${explorerTx(txHash(r))}`);
  const r2 = await call(operator, token, "storage_deposit", { account_id: operatorId }, { deposit: NEAR / 100n });
  console.log(`storage registered for operator on token  ${explorerTx(txHash(r2))}`);

  console.log("\nAdd to .env:");
  console.log(`HUB_CONTRACT=${hub}`);
  console.log(`TOKEN_CONTRACT=${token}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
