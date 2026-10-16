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
  type TransactionInstruction,
} from "@solana/web3.js";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  ExtensionType,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  createAssociatedTokenAccountIdempotentInstruction,
  createInitializeMint2Instruction,
  createInitializeTransferHookInstruction,
  createInitializeTransferFeeConfigInstruction,
  createMintToInstruction,
  getAssociatedTokenAddressSync,
  getMintLen,
} from "@solana/spl-token";
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";

const { Program, Wallet, BN } = anchor;
export { BN };

const root = new URL("..", import.meta.url).pathname;
const vaultIdl = JSON.parse(readFileSync(`${root}target/idl/gravifi_vault.json`, "utf8"));

export const VAULT_ID = new PublicKey(vaultIdl.address);
export const BPF_UPGRADEABLE = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");
export const SECONDS_PER_YEAR = 31_557_600;
export const ONE_SHARE = 1_000_000_000n;

export type Ctx = ReturnType<typeof setup>;

export function setup() {
  const svm = new LiteSVM();
  svm.addProgramFromFile(VAULT_ID, `${root}target/deploy/gravifi_vault.so`);

  const payer = Keypair.generate();
  svm.airdrop(payer.publicKey, 1_000_000_000_000n);
  const provider = new LiteSVMProvider(svm, new Wallet(payer));
  const vault = new Program(vaultIdl, provider);
  return { svm, payer, provider, vault };
}

/** Sends instructions signed by `signers` (payer always included) and expires the
 *  blockhash so an identical follow-up transaction is not rejected as a replay. */
export async function send(ctx: Ctx, ixs: TransactionInstruction[], signers: Keypair[] = []) {
  const tx = new Transaction().add(...ixs);
  await ctx.provider.sendAndConfirm!(tx, signers);
  ctx.svm.expireBlockhash();
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
) {
  const mint = Keypair.generate();
  const extensions = [...(hookProgram ? [ExtensionType.TransferHook] : []), ...(feeBps !== undefined ? [ExtensionType.TransferFeeConfig] : [])];
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
  ixs.push(createInitializeMint2Instruction(mint.publicKey, decimals, ctx.payer.publicKey, null, tokenProgram));
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
};

export { TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, Keypair, PublicKey, SystemProgram };
