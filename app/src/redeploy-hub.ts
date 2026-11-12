// One-off: deploy the built hub wasm over the existing hub account (no init, no token redeploy).
//   npm run build && npx tsx src/redeploy-hub.ts
import fs from "node:fs";
import path from "node:path";
import { agentAccount, env, view } from "./near.js";
const hub = env("HUB_CONTRACT");
const wasm = new Uint8Array(fs.readFileSync(path.resolve(process.cwd(), "../out/agenthub.wasm")));
const before = await view(hub, "get_accounting", {});
const acc = await agentAccount(hub);
const r: any = await acc.deployContract(wasm);
console.log(`deployed ${wasm.length} bytes → ${hub}  tx ${r?.transaction?.hash ?? r?.transaction_outcome?.id}`);
const after = await view(hub, "get_accounting", {});
console.log("accounting before", before, "\naccounting after ", after);
