// Thin wrapper around near-api-js for the scripts in this folder.
import "dotenv/config";
import { connect, keyStores, KeyPair, type Account, type Near } from "near-api-js";
import fs from "node:fs";
import path from "node:path";

export const NETWORK = process.env.NEAR_NETWORK ?? "testnet";
export const RPC = process.env.NEAR_RPC ?? "https://rpc.testnet.near.org";
export const KEYS_DIR = path.resolve(".keys");

export const TGAS = 1_000_000_000_000n;
export const NEAR = 1_000_000_000_000_000_000_000_000n;
export const ONE_YOCTO = 1n;

export function env(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`Missing ${name} in .env (see .env.example)`);
  return v;
}

export function usdc(amount: string | number): string {
  // 6 decimals, returned as a string for U128 JSON.
  return BigInt(Math.round(Number(amount) * 1_000_000)).toString();
}

export function fmtUsdc(raw: string | number | bigint): string {
  return (Number(raw) / 1_000_000).toFixed(2) + " USDC";
}

const keyStore = new keyStores.InMemoryKeyStore();
let nearPromise: Promise<Near> | undefined;

export async function near(): Promise<Near> {
  nearPromise ??= connect({ networkId: NETWORK, nodeUrl: RPC, keyStore });
  return nearPromise;
}

/** Account with a key from the environment, e.g. ("OPERATOR"). */
export async function accountFromEnv(prefix: string): Promise<Account> {
  const id = env(`${prefix}_ACCOUNT`);
  const key = env(`${prefix}_PRIVATE_KEY`);
  await keyStore.setKey(NETWORK, id, KeyPair.fromString(key as any));
  return (await near()).account(id);
}

/** Agent account whose key was saved by register-agent (.keys/<agent>.json). */
export async function agentAccount(agentId: string): Promise<Account> {
  const file = path.join(KEYS_DIR, `${agentId}.json`);
  if (!fs.existsSync(file)) throw new Error(`No key for ${agentId}; run: npm run register-agent -- --name <name>`);
  const { privateKey } = JSON.parse(fs.readFileSync(file, "utf8"));
  await keyStore.setKey(NETWORK, agentId, KeyPair.fromString(privateKey));
  return (await near()).account(agentId);
}

export async function view<T = any>(contractId: string, methodName: string, args: object = {}): Promise<T> {
  const n = await near();
  const acc = n.account("dontcare");
  return acc.viewFunction({ contractId, methodName, args }) as Promise<T>;
}

export async function call(
  account: Account,
  contractId: string,
  methodName: string,
  args: object = {},
  opts: { gas?: bigint; deposit?: bigint } = {},
) {
  return account.functionCall({
    contractId,
    methodName,
    args,
    gas: opts.gas ?? 100n * TGAS,
    attachedDeposit: opts.deposit ?? 0n,
  });
}

export function arg(name: string, fallback?: string): string {
  const i = process.argv.indexOf(`--${name}`);
  if (i >= 0 && process.argv[i + 1]) return process.argv[i + 1];
  if (fallback !== undefined) return fallback;
  throw new Error(`Missing --${name}`);
}

export function hasFlag(name: string): boolean {
  return process.argv.includes(`--${name}`);
}

export function explorerTx(hash: string): string {
  return `https://${NETWORK === "mainnet" ? "" : "testnet."}nearblocks.io/txns/${hash}`;
}

export function txHash(outcome: any): string {
  return outcome?.transaction?.hash ?? outcome?.transaction_outcome?.id ?? "?";
}
