// Fund a Guildhall contract from another chain through NEAR Intents (1Click API).
//
// The client sends USDC on Base / Ethereum / Arbitrum / Solana (or USDT on Tron) to a
// quote-specific deposit address; solvers deliver USDC on NEAR to the hub through
// `ft_transfer_call` with the contract's funding message (`customRecipientMsg`), so the
// contract is created in the same step. No NEAR wallet, no second hop.
//
//   npm run fund -- --from base --amount 5 --min-level 1 --client alice.near          (dry quote: price, fees, ETA)
//   npm run fund -- --from base --amount 5 --min-level 1 --client alice.near --execute (mainnet hub only: real deposit address)
//
// There is no NEAR Intents testnet. Against the testnet hub this script only ever quotes
// (the quote is real: live solver prices on mainnet assets). `customRecipientMsg` is marked
// experimental by NEAR Intents; see README "Fund from any chain".
import fs from "node:fs";
import crypto from "node:crypto";
import "dotenv/config";
import "./proxy.js";
import { arg, hasFlag, usdc, fmtUsdc } from "./near.js";

const ONECLICK = process.env.ONECLICK_URL ?? "https://1click.chaindefuser.com";
const KEY = process.env.ONECLICK_API_KEY ?? ""; // optional; without it 1Click adds a 0.25% platform fee
const NEAR_USDC = "nep141:17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1"; // Circle USDC on NEAR mainnet

/** Origin assets by short chain name, resolved against /v0/tokens at runtime (these are fallbacks). */
const ORIGINS: Record<string, { symbol: string; blockchain: string; fallback: string }> = {
  base: { symbol: "USDC", blockchain: "base", fallback: "nep141:base-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913.omft.near" },
  eth: { symbol: "USDC", blockchain: "eth", fallback: "nep141:eth-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.omft.near" },
  arb: { symbol: "USDC", blockchain: "arb", fallback: "nep141:arb-0xaf88d065e77c8cc2239327c5edb3a432268e5831.omft.near" },
  sol: { symbol: "USDC", blockchain: "sol", fallback: "nep141:sol-5ce3bf3a31af18be40ba30f721101b4341690186.omft.near" },
  tron: { symbol: "USDT", blockchain: "tron", fallback: "nep141:tron-d28a265909efecdcee7c5028585214ea0b96f015.omft.near" },
};

export interface Quote { amountIn: string; amountInFormatted: string; amountInUsd: string; amountOut: string; amountOutFormatted: string; minAmountOut: string; timeEstimate: number; depositAddress?: string; deadline?: string; }

async function tokens(): Promise<any[]> {
  const r = await fetch(`${ONECLICK}/v0/tokens`, { headers: KEY ? { "X-API-Key": KEY } : {} });
  if (!r.ok) throw new Error(`tokens ${r.status}`);
  return r.json();
}

export async function quoteFunding(opts: { from: string; amountUsdc: string; hub: string; msg: string; client: string; refundTo: string; execute: boolean }): Promise<{ quote: Quote; originAsset: string; body: any }> {
  const o = ORIGINS[opts.from]; if (!o) throw new Error(`unknown origin '${opts.from}'; use one of ${Object.keys(ORIGINS).join(", ")}`);
  let originAsset = o.fallback;
  try { const t = (await tokens()).find((x) => x.symbol === o.symbol && x.blockchain === o.blockchain); if (t) originAsset = t.assetId; } catch { /* fallback id */ }
  const body = {
    dry: !opts.execute,
    swapType: "EXACT_OUTPUT",            // the contract budget is what must arrive
    slippageTolerance: 50,                // 0.5%
    originAsset,
    depositType: "ORIGIN_CHAIN",
    destinationAsset: NEAR_USDC,
    amount: usdc(opts.amountUsdc),
    refundTo: opts.refundTo,
    refundType: "ORIGIN_CHAIN",
    recipient: opts.hub,                  // the hub contract receives the USDC...
    recipientType: "DESTINATION_CHAIN",
    customRecipientMsg: opts.msg,         // ...via ft_transfer_call(msg) → the contract is created on arrival
    deadline: new Date(Date.now() + 30 * 60_000).toISOString(),
    referral: "guildhall",
  };
  const r = await fetch(`${ONECLICK}/v0/quote`, { method: "POST", headers: { "Content-Type": "application/json", ...(KEY ? { "X-API-Key": KEY } : {}) }, body: JSON.stringify(body) });
  const j: any = await r.json();
  if (!r.ok) throw new Error(`quote ${r.status}: ${j.message ?? JSON.stringify(j).slice(0, 200)}`);
  return { quote: j.quote, originAsset, body };
}

async function main() {
  const from = arg("from", "base");
  const amount = arg("amount", "5");
  const client = arg("client", process.env.CLIENT_ACCOUNT ?? "client.near");
  const refundTo = arg("refund-to", "0x0000000000000000000000000000000000000000"); // the client's address on the origin chain
  const hub = arg("hub", process.env.HUB_CONTRACT ?? "hub.nexus0.testnet");
  const execute = hasFlag("execute");
  if (execute && hub.endsWith(".testnet")) throw new Error("--execute needs a mainnet hub: NEAR Intents has no testnet. Drop --execute for a quote.");

  const specPath = "samples/job-invoices.json";
  const hash = crypto.createHash("sha256").update(fs.readFileSync(specPath)).digest("hex");
  const msg = JSON.stringify({ skill: "document-extraction", min_level: Number(arg("min-level", "1")), min_rank: Number(arg("min-rank", "0")), require_proof: hasFlag("require-proof"), review_days: Number(arg("review-days", "5")), spec_uri: `${specPath}#sha256=${hash}`, client });

  console.log(`Fund a ${amount} USDC contract on ${hub} from ${from.toUpperCase()} (${execute ? "LIVE" : "dry quote"})`);
  const { quote, originAsset, body } = await quoteFunding({ from, amountUsdc: amount, hub, msg, client, refundTo, execute });
  console.log(`  origin asset   ${originAsset}`);
  console.log(`  you send       ${quote.amountInFormatted} (≈ $${quote.amountInUsd})`);
  console.log(`  hub receives   ${fmtUsdc(quote.amountOut)} USDC on NEAR (min ${fmtUsdc(quote.minAmountOut)} after slippage)`);
  console.log(`  solver fee     ${(Number(quote.amountInFormatted) - Number(quote.amountOutFormatted)).toFixed(4)} ${from === "tron" ? "USDT" : "USDC"}${KEY ? "" : " (includes 1Click's 0.25% unauthenticated fee)"}`);
  console.log(`  time to settle ~${quote.timeEstimate}s after your deposit confirms`);
  console.log(`  on arrival     ft_transfer_call(${hub}, ${fmtUsdc(quote.amountOut)}, msg) → contract created for ${client}`);
  if (execute) {
    console.log(`\n  DEPOSIT ADDRESS ${quote.depositAddress}\n  valid until     ${quote.deadline}\n  send exactly ${quote.amountInFormatted} from ${refundTo}; refunds go back there if the swap does not complete.`);
  } else {
    console.log(`\n  (dry) no deposit address issued. Re-run with --execute against a mainnet hub to get one.`);
  }
  fs.mkdirSync("out", { recursive: true });
  fs.writeFileSync(`out/fund-quote-${from}.json`, JSON.stringify({ request: body, quote }, null, 2));
}

if (process.argv[1]?.endsWith("fund.ts")) main().catch((e) => { console.error(e.message ?? e); process.exit(1); });
