// Checks the README's claims against the live hub. Read-only: no keys, no .env, no tokens.
//   npm run verify                          (defaults to hub.nexus0.testnet)
//   HUB_CONTRACT=hub.you.testnet npm run verify
// Exits non-zero if any check fails.
import fs from "node:fs";
import crypto from "node:crypto";
import { near, view, fmtUsdc } from "./near.js";
import { recoverSigner } from "./nearai.js";

const hub = process.env.HUB_CONTRACT ?? "hub.nexus0.testnet";
const ALLOWED = new Set(["accept_job", "submit", "withdraw", "post_note"]);
let failed = 0;
let warned = 0;
const ok = (msg: string) => console.log(`  ✔ ${msg}`);
const bad = (msg: string) => { failed++; console.log(`  ✘ ${msg}`); };
const warn = (msg: string) => { warned++; console.log(`  – ${msg}`); };
const short = (a: string) => a.split(".")[0];

async function main() {
  const stats = await view(hub, "get_stats");
  console.log(`hub ${hub}  token ${stats.token}  fee ${stats.fee_bps / 100}%  members ${stats.agents}  contracts ${stats.jobs}\n`);

  console.log("1. The books balance (every token the hub holds is accounted for)");
  const acc = await view(hub, "get_accounting");
  const held = await view(stats.token, "ft_balance_of", { account_id: hub });
  const line = `escrowed ${fmtUsdc(acc.escrowed)} + member balances ${fmtUsdc(acc.member_balances)} + fees ${fmtUsdc(acc.fees)} + in flight ${fmtUsdc(acc.in_flight)} + refunds owed ${fmtUsdc(acc.refunds_owed)} = ${fmtUsdc(acc.total)}`;
  if (BigInt(acc.total) === BigInt(held)) ok(`${line}; token contract says the hub holds ${fmtUsdc(held)}`);
  else bad(`${line}, but the token contract says the hub holds ${fmtUsdc(held)}`);

  console.log("\n2. Every member account has exactly one key, and it can only call the hub");
  const agents: any[] = await view(hub, "list_agents", { limit: 200 });
  const n = await near();
  for (const a of agents) {
    const keys: any[] = await (await n.account(a.account)).getAccessKeys();
    const perm = keys[0]?.access_key?.permission;
    const fc = perm && typeof perm === "object" ? perm.FunctionCall : null;
    const methods: string[] = fc?.method_names ?? [];
    if (keys.length === 1 && fc && fc.receiver_id === hub && methods.length > 0 && methods.every((m) => ALLOWED.has(m))) {
      ok(`${short(a.account)}: 1 key, function-call, ${hub} only, methods ${methods.join(", ")}`);
    } else {
      bad(`${short(a.account)}: ${keys.length} key(s), ${fc ? `receiver ${fc.receiver_id}, methods ${methods.join(", ") || "ANY"}` : "FULL ACCESS"}`);
    }
  }

  console.log("\n3. Seals are NFTs on the member account, each linked to a scoring report in this repo");
  for (const a of agents) {
    const certs: any[] = await view(hub, "get_certs", { agent: a.account });
    const nfts: any[] = await view(hub, "nft_tokens_for_owner", { account_id: a.account });
    const revs: any[] = await view(hub, "get_revocations", { agent: a.account }).catch(() => []);
    if (certs.length !== nfts.length) { bad(`${short(a.account)}: ${certs.length} certs but ${nfts.length} NFTs`); continue; }
    const missing = certs.map((c) => String(c.score_uri).split("/app/out/")[1]).filter((f) => f && !fs.existsSync(`out/${f}`));
    const msg = `${short(a.account)}: ${certs.length} seal(s) on record, ${revs.length} burned`;
    if (missing.length) warn(`${msg}; report not in this checkout: ${[...new Set(missing)].join(", ")}`);
    else ok(`${msg}; every linked report is in app/out`);
  }

  console.log("\n4. Delivered work matches the hash the member put on-chain");
  const jobs: any[] = await view(hub, "list_jobs", { limit: 500 });
  for (const j of jobs) {
    if (!j.output_uri) continue;
    const [file, frag] = String(j.output_uri).split("#sha256=");
    if (file.endsWith("attack-demo.json")) { warn(`contract #${j.id} (${j.status}): escrow test from \`npm run attack:escrow\`, placeholder deliverable by design`); continue; }
    if (!fs.existsSync(file)) { warn(`contract #${j.id}: ${file} is not in this checkout (on-chain hash ${String(frag).slice(0, 12)}…)`); continue; }
    const hash = crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
    if (hash === frag) ok(`contract #${j.id} (${j.status}, ${short(j.agent)}): ${file} sha256 ${hash.slice(0, 12)}… matches`);
    else bad(`contract #${j.id}: ${file} hashes to ${hash.slice(0, 12)}…, chain says ${String(frag).slice(0, 12)}…`);
  }

  console.log("\n5. TEE execution proofs recover to a signer the hub trusts (checked here and by the contract)");
  const signers: [string, string][] = await view(hub, "list_trusted_signers");
  const trusted = new Set(signers.map((s) => s[0].replace(/^0x/, "").toLowerCase()));
  let proofs = 0;
  for (const j of jobs) {
    const p = j.proof;
    if (!p) continue;
    proofs++;
    const text = p.signature_kind === "provider_tee" ? `${p.model}:${p.request_hash}:${p.response_hash}` : `${p.request_hash}:${p.response_hash}`;
    const local = recoverSigner(text, p.signature).replace(/^0x/, "");
    const [onchain, why] = await view(hub, "verify_proof", { proof: p });
    if (onchain && trusted.has(local) && local === String(p.signing_address).replace(/^0x/, "").toLowerCase()) ok(`contract #${j.id}: ${p.model}, signer 0x${local.slice(0, 8)}… — ${why}`);
    else bad(`contract #${j.id}: local signer 0x${local.slice(0, 8)}…, contract says ${onchain ? "valid" : `invalid (${why})`}`);
  }
  if (!proofs) warn("no contract on this hub carries a proof yet");

  console.log(`\n${failed ? `FAILED: ${failed} check(s)` : "ALL CHECKS PASSED"}${warned ? ` (${warned} note(s) marked –)` : ""}`);
  process.exit(failed ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(1); });
