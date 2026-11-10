// Shared harness for program tests. Runs the vault inside LiteSVM, so tests are fast,
// deterministic, and can move the clock, which fee accrual and rebalancing depend on.
import anchor from "@coral-xyz/anchor";
import { LiteSVMProvider } from "anchor-litesvm";
import { Clock, LiteSVM } from "litesvm";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  ExtensionType,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  createAssociatedTokenAccountIdempotentInstruction,
  createInitializeDefaultAccountStateInstruction,
  createInitializeMint2Instruction,
  createInitializePausableConfigInstruction,
  createInitializeTransferHookInstruction,
  AccountState,
  createInitializeTransferFeeConfigInstruction,
  createMintToInstruction,
  getAssociatedTokenAddressSync,
  getMintLen,
} from "@solana/spl-token";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import assert from "node:assert/strict";

const { Program, Wallet, BN } = anchor;
export { BN };

/** A creator's announced weights in a basket's extension account (set_targets, record 3:
 *  [due i64][fingerprint 32][targets u16 x14]), or null. Read here, not from the app, so the
 *  program's tests run on their own. */
function readPendingTargets(ext: Uint8Array | null | undefined, count: number): { due: number; fingerprint: Uint8Array; bps: number[] } | null {
  const RECORDS_AT = 44, USED_AT = 42, MAX = 14, TAG = 3;
  if (!ext || ext.length < RECORDS_AT) return null;
  const dv = new DataView(ext.buffer, ext.byteOffset, ext.byteLength);
  const used = dv.getUint16(USED_AT, true);
  for (let at = 0; at + 4 <= used; at += 4 + dv.getUint16(RECORDS_AT + at + 2, true)) {
    if (dv.getUint16(RECORDS_AT + at, true) !== TAG || dv.getUint16(RECORDS_AT + at + 2, true) < 40 + 2 * MAX) continue;
    const o = RECORDS_AT + at + 4;
    const due = Number(dv.getBigInt64(o, true));
    if (due === 0) return null;
    return { due, fingerprint: ext.slice(o + 8, o + 40), bps: Array.from({ length: Math.min(count, MAX) }, (_, i) => dv.getUint16(o + 40 + 2 * i, true)) };
  }
  return null;
}

const root = new URL("..", import.meta.url).pathname;
const vaultIdl = JSON.parse(readFileSync(`${root}target/idl/gravifi_vault.json`, "utf8"));

export const VAULT_ID = new PublicKey(vaultIdl.address);
export const BPF_UPGRADEABLE = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");
export const SECONDS_PER_YEAR = 31_557_600;
export const ONE_SHARE = 1_000_000_000n;

export type Ctx = ReturnType<typeof setup>;

/** A fresh LiteSVM with the vault program: this build, or the binary at `program` (an upgrade
 *  rehearsal starts from the one on mainnet, then loads this build over it). */
export function setup(program = `${root}target/deploy/gravifi_vault.so`) {
  const svm = new LiteSVM();
  svm.addProgramFromFile(VAULT_ID, program);

  const payer = Keypair.generate();
  svm.airdrop(payer.publicKey, 1_000_000_000_000n);
  const provider = new LiteSVMProvider(svm, new Wallet(payer));
  const vault = new Program(vaultIdl, provider);
  return { svm, payer, provider, vault };
}

/** Sends instructions signed by `signers` (payer always included) and expires the
 *  blockhash so an identical follow-up transaction is not rejected as a replay. */
export async function send(ctx: Ctx, ixs: TransactionInstruction[], signers: Keypair[] = []) {
  const announced = await withNotice(ctx, ixs);
  const tx = new Transaction().add(...announced.ixs);
  await ctx.provider.sendAndConfirm!(tx, signers);
  ctx.svm.expireBlockhash();
  for (const basket of announced.baskets) await applyWhenDue(ctx, basket);
}

// A creator's new weights on a basket others hold shares in are announced and apply a day later
// (set_targets, apply_targets). Tests written before that see them applied as before: a set_targets
// sent without remaining accounts gets the basket's extension account (created first if needed),
// and once it lands the announcement is applied at its due time, the clock then put back. Tests of
// the notice itself opt out (`manualNotice`) and name the accounts themselves.
const SET_TARGETS = createHash("sha256").update("global:set_targets").digest().subarray(0, 8);
const manual = new WeakSet<object>();
export const manualNotice = (ctx: Ctx) => manual.add(ctx);
const extOf = (ctx: Ctx, basket: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("ext"), basket.toBuffer()], ctx.vault.programId)[0];

async function withNotice(ctx: Ctx, ixs: TransactionInstruction[]) {
  if (manual.has(ctx)) return { ixs, baskets: [] as PublicKey[] };
  const out: TransactionInstruction[] = [];
  const baskets: PublicKey[] = [];
  for (const ix of ixs) {
    if (!ix.programId.equals(ctx.vault.programId) || !ix.data.subarray(0, 8).equals(SET_TARGETS) || ix.keys.length !== 3) {
      out.push(ix);
      continue;
    }
    const [authority, basket] = [ix.keys[0].pubkey, ix.keys[1].pubkey];
    const ext = extOf(ctx, basket);
    if (!ctx.svm.getAccount(ext) && !baskets.some((b) => b.equals(basket))) out.push(await ctx.vault.methods.extInit().accountsPartial({ payer: authority, basket, ext }).instruction());
    out.push(new TransactionInstruction({ programId: ix.programId, data: ix.data, keys: [...ix.keys, { pubkey: ext, isSigner: false, isWritable: true }] }));
    baskets.push(basket);
  }
  return { ixs: out, baskets };
}

async function applyWhenDue(ctx: Ctx, basket: PublicKey) {
  const ext = extOf(ctx, basket);
  const acc = ctx.svm.getAccount(ext);
  const pending = acc ? readPendingTargets(acc.data, 14) : null;
  if (!pending) return;
  const c = ctx.svm.getClock();
  ctx.svm.setClock(new Clock(c.slot, c.epochStartTimestamp, c.epoch, c.leaderScheduleEpoch, BigInt(pending.due)));
  try {
    const ix = await ctx.vault.methods.applyTargets().accountsPartial({ basket, ext }).instruction();
    await ctx.provider.sendAndConfirm!(new Transaction().add(ix), []);
  } finally {
    ctx.svm.setClock(c);
    ctx.svm.expireBlockhash();
  }
}

/** Runs an Anchor method builder and expires the blockhash afterwards. */
export async function rpc(ctx: Ctx, builder: any, signers: Keypair[] = []) {
  const ix = await builder.instruction();
  await send(ctx, [ix], signers);
}

/** Asserts the call fails, and that the program logs mention `code`. */
export async function expectError(p: Promise<unknown>, code: string) {
  try {
    await p;
  } catch (e: any) {
    const text = [e?.message ?? "", ...(e?.logs ?? []), ...(e?.transactionLogs ?? [])].join("\n");
    assert.ok(text.includes(code), `expected error "${code}", got:\n${text}`);
    return;
  }
  assert.fail(`expected error "${code}", but the transaction succeeded`);
}

export function fund(ctx: Ctx, lamports = 100_000_000_000n) {
  const kp = Keypair.generate();
  ctx.svm.airdrop(kp.publicKey, lamports);
  return kp;
}

export function warp(ctx: Ctx, seconds: number) {
  const c = ctx.svm.getClock();
  ctx.svm.setClock(
    new Clock(c.slot + 1n, c.epochStartTimestamp, c.epoch, c.leaderScheduleEpoch, c.unixTimestamp + BigInt(seconds)),
  );
  ctx.svm.expireBlockhash();
}

export function now(ctx: Ctx) {
  return Number(ctx.svm.getClock().unixTimestamp);
}

/** Creates a mint. Pass `hookProgram` to give a Token-2022 mint an enabled transfer hook, or
 *  `feeBps` to have it charge a transfer fee (the payer is its fee authority). */
export async function createMint(
  ctx: Ctx,
  decimals: number,
  tokenProgram = TOKEN_PROGRAM_ID,
  hookProgram?: PublicKey,
  feeBps?: number,
  /** As issuers of tokenized stocks set them up: the payer can freeze accounts, pause the
   *  whole mint (Token-2022), or have new accounts start frozen (Token-2022). */
  issuer: { freezable?: boolean; pausable?: boolean; startsFrozen?: boolean } = {},
) {
  const mint = Keypair.generate();
  const extensions = [
    ...(hookProgram ? [ExtensionType.TransferHook] : []),
    ...(feeBps !== undefined ? [ExtensionType.TransferFeeConfig] : []),
    ...(issuer.pausable ? [ExtensionType.PausableConfig] : []),
    ...(issuer.startsFrozen ? [ExtensionType.DefaultAccountState] : []),
  ];
  const space = getMintLen(extensions);
  const lamports = Number(ctx.svm.minimumBalanceForRentExemption(BigInt(space)));
  const ixs: TransactionInstruction[] = [
    SystemProgram.createAccount({
      fromPubkey: ctx.payer.publicKey,
      newAccountPubkey: mint.publicKey,
      space,
      lamports,
      programId: tokenProgram,
    }),
  ];
  if (hookProgram) {
    ixs.push(createInitializeTransferHookInstruction(mint.publicKey, ctx.payer.publicKey, hookProgram, tokenProgram));
  }
  if (feeBps !== undefined) {
    ixs.push(createInitializeTransferFeeConfigInstruction(mint.publicKey, ctx.payer.publicKey, ctx.payer.publicKey, feeBps, 2n ** 63n, tokenProgram));
  }
  if (issuer.pausable) ixs.push(createInitializePausableConfigInstruction(mint.publicKey, ctx.payer.publicKey, tokenProgram));
  if (issuer.startsFrozen) ixs.push(createInitializeDefaultAccountStateInstruction(mint.publicKey, AccountState.Frozen, tokenProgram));
  const freeze = issuer.freezable || issuer.startsFrozen ? ctx.payer.publicKey : null;
  ixs.push(createInitializeMint2Instruction(mint.publicKey, decimals, ctx.payer.publicKey, freeze, tokenProgram));
  await send(ctx, ixs, [mint]);
  return mint.publicKey;
}

export function ata(mint: PublicKey, owner: PublicKey, tokenProgram = TOKEN_PROGRAM_ID) {
  return getAssociatedTokenAddressSync(mint, owner, true, tokenProgram, ASSOCIATED_TOKEN_PROGRAM_ID);
}

export async function createAta(ctx: Ctx, mint: PublicKey, owner: PublicKey, tokenProgram = TOKEN_PROGRAM_ID) {
  const address = ata(mint, owner, tokenProgram);
  await send(ctx, [
    createAssociatedTokenAccountIdempotentInstruction(ctx.payer.publicKey, address, owner, mint, tokenProgram),
  ]);
  return address;
}

export async function mintTo(ctx: Ctx, mint: PublicKey, dest: PublicKey, amount: bigint, tokenProgram = TOKEN_PROGRAM_ID) {
  await send(ctx, [createMintToInstruction(mint, dest, ctx.payer.publicKey, amount, [], tokenProgram)]);
}

/** Token balance straight from account bytes: amount lives at offset 64 for both programs. */
export function balance(ctx: Ctx, tokenAccount: PublicKey): bigint {
  const acc = ctx.svm.getAccount(tokenAccount);
  if (!acc) return 0n;
  return Buffer.from(acc.data).readBigUInt64LE(64);
}

/** Supply straight from mint bytes: offset 36. */
export function supply(ctx: Ctx, mint: PublicKey): bigint {
  const acc = ctx.svm.getAccount(mint)!;
  return Buffer.from(acc.data).readBigUInt64LE(36);
}

/**
 * LiteSVM loads programs without an upgradeable-loader ProgramData account, so write the
 * one `init_protocol` checks: variant 3, slot, Some(authority).
 */
export function fakeProgramData(ctx: Ctx, authority: PublicKey) {
  const [address] = PublicKey.findProgramAddressSync([VAULT_ID.toBuffer()], BPF_UPGRADEABLE);
  const data = Buffer.alloc(45);
  data.writeUInt32LE(3, 0);
  data.writeBigUInt64LE(0n, 4);
  data.writeUInt8(1, 12);
  authority.toBuffer().copy(data, 13);
  ctx.svm.setAccount(address, {
    lamports: Number(ctx.svm.minimumBalanceForRentExemption(45n)),
    data,
    owner: BPF_UPGRADEABLE,
    executable: false,
  });
  return address;
}

export const pda = {
  protocol: () => PublicKey.findProgramAddressSync([Buffer.from("protocol")], VAULT_ID)[0],
  burn: () => PublicKey.findProgramAddressSync([Buffer.from("burn")], VAULT_ID)[0],
  asset: (mint: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("asset"), mint.toBuffer()], VAULT_ID)[0],
  basket: (creator: PublicKey, id: Uint8Array) =>
    PublicKey.findProgramAddressSync([Buffer.from("basket"), creator.toBuffer(), Buffer.from(id)], VAULT_ID)[0],
  share: (basket: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("share"), basket.toBuffer()], VAULT_ID)[0],
  allow: (basket: PublicKey, wallet: PublicKey) =>
    PublicKey.findProgramAddressSync([Buffer.from("allow"), basket.toBuffer(), wallet.toBuffer()], VAULT_ID)[0],
  keepers: () => PublicKey.findProgramAddressSync([Buffer.from("keepers")], VAULT_ID)[0],
  price: (mint: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("price"), mint.toBuffer()], VAULT_ID)[0],
  rebalance: (basket: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("rebalance"), basket.toBuffer()], VAULT_ID)[0],
  buyback: () => PublicKey.findProgramAddressSync([Buffer.from("buyback")], VAULT_ID)[0],
  creationBurn: () => PublicKey.findProgramAddressSync([Buffer.from("creation_burn")], VAULT_ID)[0],
  guard: (mint: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("guard"), mint.toBuffer()], VAULT_ID)[0],
  invite: (wallet: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("invite"), wallet.toBuffer()], VAULT_ID)[0],
  lister: () => PublicKey.findProgramAddressSync([Buffer.from("lister")], VAULT_ID)[0],
  strategist: () => PublicKey.findProgramAddressSync([Buffer.from("strategist")], VAULT_ID)[0],
  parts: () => PublicKey.findProgramAddressSync([Buffer.from("parts")], VAULT_ID)[0],
  ext: (basket: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("ext"), basket.toBuffer()], VAULT_ID)[0],
  trip: (basket: PublicKey, id: number) => {
    const n = Buffer.alloc(4);
    n.writeUInt32LE(id);
    return PublicKey.findProgramAddressSync([Buffer.from("trip"), basket.toBuffer(), n], VAULT_ID)[0];
  },
  tripIou: (trip: PublicKey, wallet: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("trip_iou"), trip.toBuffer(), wallet.toBuffer()], VAULT_ID)[0],
  smart: (basket: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("smart"), basket.toBuffer()], VAULT_ID)[0],
  verified: (mint: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("verified"), mint.toBuffer()], VAULT_ID)[0],
  stop: (basket: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("stop"), basket.toBuffer()], VAULT_ID)[0],
  cutNotice: (basket: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("cut_notice"), basket.toBuffer()], VAULT_ID)[0],
  stakePool: () => PublicKey.findProgramAddressSync([Buffer.from("stake_pool")], VAULT_ID)[0],
  stakeRewards: () => PublicKey.findProgramAddressSync([Buffer.from("stake_rewards")], VAULT_ID)[0],
  stake: (wallet: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("stake"), wallet.toBuffer()], VAULT_ID)[0],
  shareEscrow: () => PublicKey.findProgramAddressSync([Buffer.from("share_escrow")], VAULT_ID)[0],
  shareStake: (basket: PublicKey, wallet: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("share_stake"), basket.toBuffer(), wallet.toBuffer()], VAULT_ID)[0],
  community: (basket: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("community"), basket.toBuffer()], VAULT_ID)[0],
  proposal: (basket: PublicKey, id: number) => {
    const n = Buffer.alloc(4);
    n.writeUInt32LE(id);
    return PublicKey.findProgramAddressSync([Buffer.from("proposal"), basket.toBuffer(), n], VAULT_ID)[0];
  },
  vote: (proposal: PublicKey, wallet: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("vote"), proposal.toBuffer(), wallet.toBuffer()], VAULT_ID)[0],
};

/** How long after an asset's first price (which only seeds its guard) a post that agrees with it
 *  makes the price usable: a quarter of the hour's reference window. */
export const PRICE_SEED_SECS = 900;

/** Whether an asset has a usable posted price (a first post only seeds its guard). */
export function priced(ctx: Ctx, mint: PublicKey) {
  const acc = ctx.svm.getAccount(pda.price(mint));
  return !!acc && BigInt(ctx.vault.coder.accounts.decode("assetPrice", Buffer.from(acc.data)).priceE12.toString()) > 0n;
}

/** Seeds the guard of each asset here with no usable price yet (its first post), then lets the
 *  quarter hour pass, so the next post of that price is usable. Prices posted before it age by it. */
export async function seedPrices(ctx: Ctx, keeper: Keypair, prices: [PublicKey, bigint][]) {
  const unseeded = prices.filter(([mint]) => !priced(ctx, mint));
  for (const [mint, price] of unseeded) {
    await rpc(
      ctx,
      ctx.vault.methods.setPrice(new BN(price.toString())).accountsPartial({ priceAuthority: keeper.publicKey, keepers: pda.keepers(), mint, assetPrice: pda.price(mint), pythPrice: null }),
      [keeper],
    );
  }
  if (unseeded.length) warp(ctx, PRICE_SEED_SECS);
}

/** Posts prices as the keeper does once they're bounded: an asset with no usable price yet is
 *  seeded first, and confirmed a quarter of an hour later; a price more than 20% from its guard's
 *  reference is approached in steps of 20%, an hour apart, then every price is posted again at
 *  the end so all are fresh together. */
export async function postPrices(ctx: Ctx, keeper: Keypair, prices: [PublicKey, bigint][]) {
  const post = (mint: PublicKey, price: bigint) =>
    rpc(
      ctx,
      ctx.vault.methods.setPrice(new BN(price.toString())).accountsPartial({ priceAuthority: keeper.publicKey, keepers: pda.keepers(), mint, assetPrice: pda.price(mint), pythPrice: null }),
      [keeper],
    );
  await seedPrices(ctx, keeper, prices);
  for (;;) {
    let stepped = false;
    for (const [mint, target] of prices) {
      const acc = ctx.svm.getAccount(pda.guard(mint));
      const ref = acc ? BigInt(ctx.vault.coder.accounts.decode("priceGuard", Buffer.from(acc.data)).referenceE12.toString()) : 0n;
      if (ref === 0n || (target > ref ? target - ref : ref - target) * 10_000n <= ref * 2_000n) continue;
      await post(mint, target > ref ? (ref * 12_000n) / 10_000n : (ref * 8_000n) / 10_000n);
      stepped = true;
    }
    if (!stepped) break;
    warp(ctx, 3_601);
  }
  for (const [mint, price] of prices) await post(mint, price);
}

export { TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, Keypair, PublicKey, SystemProgram };
