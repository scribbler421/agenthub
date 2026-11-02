// NEAR AI Cloud backend: inference inside a TEE, with a signature over the
// exact request/response bytes that the hub contract can verify on-chain.
//
// Flow (docs.near.ai/cloud/verification):
//   1. GET /v1/attestation/report?model=&provider=near&nonce=  → attested signing addresses
//   2. POST /v1/chat/completions (non-streaming, x-no-aliasing) → chat id + raw body
//   3. GET /v1/signature/{id}?signing_algo=ecdsa → text, signature, signing_address
//      text = "<model>:<sha256(request)>:<sha256(response)>", EIP-191 ECDSA
//   4. Verify locally (recover signer, match attestation); the contract repeats
//      the recovery with its native ecrecover against owner-registered signers.
import "dotenv/config";
import "./proxy.js";
import crypto from "node:crypto";
import { keccak_256 } from "@noble/hashes/sha3.js";
import { secp256k1 } from "@noble/curves/secp256k1.js";

const BASE = process.env.NEARAI_BASE_URL ?? "https://cloud-api.near.ai";
const KEY = process.env.NEARAI_CLOUD_API_KEY ?? "";
export const NEARAI_MODEL = process.env.NEARAI_MODEL ?? "zai-org/GLM-5.1-FP8";

export interface ExecutionProof {
  model: string;
  chat_id: string;
  request_hash: string; // hex sha256 of request body bytes
  response_hash: string; // hex sha256 of response body bytes
  signature: string; // 65-byte hex (r,s,v), EIP-191 over `text`
  signing_address: string; // 0x… ethereum-style address of the TEE signer
  signature_kind: "provider_tee" | "gateway";
  text: string;
}

const sha256 = (b: Uint8Array) => crypto.createHash("sha256").update(b).digest("hex");

function headers(extra: Record<string, string> = {}) {
  if (!KEY) throw new Error("NEARAI_CLOUD_API_KEY missing in .env");
  return { Authorization: `Bearer ${KEY}`, Accept: "application/json", ...extra };
}

/** Fetch a model attestation bound to a fresh nonce; returns the attested TEE signer addresses. */
export async function attest(model = NEARAI_MODEL): Promise<{ nonce: string; model_signers: string[]; gateway_signer?: string; raw: any }> {
  const nonce = crypto.randomBytes(32).toString("hex");
  const url = `${BASE}/v1/attestation/report?model=${encodeURIComponent(model)}&provider=near&nonce=${nonce}&signing_algo=ecdsa`;
  const res = await fetch(url, { headers: headers() });
  if (!res.ok) throw new Error(`attestation ${res.status}: ${(await res.text()).slice(0, 300)}`);
  const raw = await res.json();
  const cands: any[] = raw.model_attestations ?? [];
  if (!cands.length) throw new Error("attestation: no model_attestations returned");
  const model_signers = cands.map((c) => String(c.signing_address ?? c.signer ?? "").toLowerCase()).filter(Boolean);
  const echoed = JSON.stringify(raw).includes(nonce);
  if (!echoed) throw new Error("attestation: nonce not echoed (possible replay)");
  return { nonce, model_signers, gateway_signer: raw.signing_address?.toLowerCase(), raw };
}

/** One chat completion inside the TEE. Returns the model's text plus a verifiable proof. */
export async function completeSigned(system: string, user: string, model = NEARAI_MODEL, maxTokens = 2000): Promise<{ text: string; proof: ExecutionProof; body: any }> {
  const reqBody = Buffer.from(JSON.stringify({
    model,
    messages: [{ role: "system", content: system }, { role: "user", content: user }],
    max_tokens: maxTokens,
    temperature: 0,
    stream: false,
  }));
  const res = await fetch(`${BASE}/v1/chat/completions`, {
    method: "POST",
    headers: headers({ "Content-Type": "application/json", "x-no-aliasing": "true", "Accept-Encoding": "identity" }),
    body: reqBody,
  });
  const resBytes = new Uint8Array(await res.arrayBuffer());
  if (!res.ok) throw new Error(`chat ${res.status}: ${Buffer.from(resBytes).toString("utf8").slice(0, 300)}`);
  const body = JSON.parse(Buffer.from(resBytes).toString("utf8"));
  const chat_id: string = body.id;
  const text: string = body.choices?.[0]?.message?.content ?? "";
  const request_hash = sha256(reqBody);
  const response_hash = sha256(resBytes);

  // Signature can lag the completion by a moment.
  let sig: any = null;
  for (let i = 0; i < 8 && !sig; i++) {
    const r = await fetch(`${BASE}/v1/signature/${encodeURIComponent(chat_id)}?signing_algo=ecdsa`, { headers: headers() });
    if (r.status === 404) { await new Promise((d) => setTimeout(d, 1000)); continue; }
    const j = await r.json();
    if (j.error_code) throw new Error(`signature unavailable: ${j.message}`);
    sig = j;
  }
  if (!sig) throw new Error(`no signature for ${chat_id} after retries`);

  const proof: ExecutionProof = {
    model, chat_id, request_hash, response_hash,
    signature: sig.signature, signing_address: String(sig.signing_address).toLowerCase(),
    signature_kind: sig.signature_kind, text: sig.text,
  };
  const expected = proof.signature_kind === "provider_tee" ? `${model}:${request_hash}:${response_hash}` : `${request_hash}:${response_hash}`;
  if (proof.text !== expected) throw new Error(`signed text mismatch:\n got ${proof.text}\n want ${expected}`);
  if (recoverSigner(proof.text, proof.signature) !== proof.signing_address) throw new Error("signature does not recover to signing_address");
  return { text, proof, body };
}

/** EIP-191 personal_sign recovery → lowercase 0x address. Same math the contract does with ecrecover. */
export function recoverSigner(text: string, signatureHex: string): string {
  const msg = Buffer.from(text, "utf8");
  const prefixed = Buffer.concat([Buffer.from(`\x19Ethereum Signed Message:\n${msg.length}`, "utf8"), msg]);
  const digest = keccak_256(prefixed);
  const sigBytes = Buffer.from(signatureHex.replace(/^0x/, ""), "hex");
  if (sigBytes.length !== 65) throw new Error("signature must be 65 bytes");
  let v = sigBytes[64];
  if (v >= 27) v -= 27;
  const recovered = new Uint8Array(65); recovered[0] = v; recovered.set(sigBytes.subarray(0, 64), 1);
  const pub = secp256k1.Signature.fromBytes(recovered, "recovered").recoverPublicKey(digest).toBytes(false).subarray(1); // drop 0x04
  return "0x" + Buffer.from(keccak_256(pub)).toString("hex").slice(-40);
}

/** Verify a proof end to end against a fresh attestation. */
export async function verifyProof(proof: ExecutionProof): Promise<{ ok: boolean; reason: string }> {
  try {
    if (recoverSigner(proof.text, proof.signature) !== proof.signing_address) return { ok: false, reason: "signature does not match signing_address" };
    const a = await attest(proof.model);
    if (proof.signature_kind === "provider_tee" && !a.model_signers.includes(proof.signing_address)) return { ok: false, reason: "signer is not an attested TEE for this model" };
    if (proof.signature_kind === "gateway" && a.gateway_signer !== proof.signing_address) return { ok: false, reason: "signer is not the attested gateway" };
    return { ok: true, reason: `${proof.signature_kind} signature by attested signer ${proof.signing_address}` };
  } catch (e: any) {
    return { ok: false, reason: e.message };
  }
}
