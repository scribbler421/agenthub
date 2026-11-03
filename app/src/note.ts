// The contract's thread. Client or member writes a line; anyone reads it.
//   npm run note -- --job 3 --text "Only the first document on each page counts."   (as CLIENT)
//   npm run note -- --job 3 --as extractor --text "Understood."                      (as a member, with its scoped key)
//   npm run note -- --job 3                                                           (read the thread)
import { accountFromEnv, agentAccount, call, env, arg, view, txHash, explorerTx } from "./near.js";

async function main() {
  const hub = env("HUB_CONTRACT");
  const jobId = Number(arg("job"));
  const text = process.argv.includes("--text") ? arg("text") : "";
  if (text) {
    const as = process.argv.includes("--as") ? arg("as") : "";
    const signer = as ? await agentAccount(`${as}.${hub}`) : await accountFromEnv("CLIENT");
    const r = await call(signer, hub, "post_note", { job_id: jobId, text });
    console.log(`posted as ${signer.accountId}  ${explorerTx(txHash(r))}`);
  }
  const notes: any[] = await view(hub, "get_notes", { job_id: jobId });
  console.log(`contract #${jobId} thread (${notes.length}):`);
  for (const n of notes) console.log(`  [${new Date(Number(n.at) / 1e6).toISOString().slice(0, 16)}] ${n.author.split(".")[0]}: ${n.text}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
