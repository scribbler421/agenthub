// The agent runner. Signs ONLY with the agent's scoped function-call key.
// Polls open jobs it is certified for, accepts, does the extraction, submits.
//   npm run agent -- --name extractor            (runs until no open jobs, or --watch)
import fs from "node:fs";
import crypto from "node:crypto";
import { agentAccount, call, env, arg, hasFlag, view, txHash, explorerTx, fmtUsdc } from "./near.js";
import { extractWithProof, PROVIDER, MODEL_NAME } from "./extract.js";

const SKILL = "document-extraction";
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function work(hub: string, agentId: string): Promise<number> {
  const agent = await agentAccount(agentId);
  const open: any[] = await view(hub, "list_jobs", { status: "Open" });
  let done = 0;
  for (const job of open) {
    if (job.skill !== SKILL) continue;
    const ok = await view(hub, "is_certified", { agent: agentId, skill: SKILL, min_level: job.min_level });
    if (!ok) { console.log(`job #${job.id}: needs level ${job.min_level}, skipping`); continue; }

    console.log(`job #${job.id}: ${fmtUsdc(job.budget)} — accepting`);
    const a = await call(agent, hub, "accept_job", { job_id: job.id });
    console.log(`  accepted  ${explorerTx(txHash(a))}`);

    // Resolve the spec. v1: "<path>#sha256=<hash>" in this repo.
    const [specPath, frag] = String(job.spec_uri).split("#");
    const specBytes = fs.readFileSync(specPath);
    const hash = crypto.createHash("sha256").update(specBytes).digest("hex");
    if (frag && frag !== `sha256=${hash}`) throw new Error(`spec hash mismatch for job #${job.id}`);
    const spec = JSON.parse(specBytes.toString("utf8")) as { documents: { id: string; text: string }[] };

    if (job.require_proof && PROVIDER !== "nearai") { console.log(`job #${job.id}: requires a TEE proof but this agent runs on ${PROVIDER}; skipping`); continue; }

    const results = [];
    for (const d of spec.documents) {
      const { fields, proof } = await extractWithProof(d.text);
      results.push({ id: d.id, fields, proof });
      console.log(`  ${d.id}: ${fields.vendor.value} / ${fields.invoice_number.value} / ${fields.total.value} ${fields.currency.value}${proof ? `  [TEE ${proof.signature_kind} ${proof.signing_address.slice(0, 10)}…]` : ""}`);
    }

    fs.mkdirSync("out", { recursive: true });
    const outPath = `out/job-${job.id}.json`;
    const body = JSON.stringify({ job_id: job.id, agent: agentId, provider: PROVIDER, model: MODEL_NAME, results }, null, 2);
    fs.writeFileSync(outPath, body);
    const outHash = crypto.createHash("sha256").update(body).digest("hex");
    // One proof goes on-chain: the last document's (every per-document proof is in the output file).
    const proof = results.map((r) => r.proof).filter(Boolean).at(-1) ?? null;
    const s = await call(agent, hub, "submit", { job_id: job.id, output_uri: `${outPath}#sha256=${outHash}`, proof });
    console.log(`  submitted ${outPath}${proof ? " with TEE proof (verified on-chain)" : ""}  ${explorerTx(txHash(s))}`);
    done++;
  }
  return done;
}

async function main() {
  const hub = env("HUB_CONTRACT");
  const agentId = `${arg("name")}.${hub}`;
  console.log(`agent ${agentId} online (key scoped to ${hub}: accept_job, submit, withdraw)`);
  do {
    const n = await work(hub, agentId);
    if (!hasFlag("watch")) { console.log(n ? `done: ${n} job(s)` : "no open jobs"); break; }
    await sleep(5000);
  } while (true);
}

main().catch((e) => { console.error(e); process.exit(1); });
