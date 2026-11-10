// Fees in USDC, for tests: a USDC mint at its mainnet address, the protocol's USDC accounts, the
// fee accounts a flow passes after its quads, and the price key's signed value (`fee_quote`).
import { Keypair, PublicKey, SYSVAR_INSTRUCTIONS_PUBKEY, type TransactionInstruction } from "@solana/web3.js";
import { MintLayout, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { BN, ata, createAta, pda, type Ctx } from "./helpers.ts";
import { USDC } from "./fixtures.ts";

/** Puts a USDC mint at its mainnet address, the test's payer its mint authority. */
export function usdcMint(ctx: Ctx) {
  const data = Buffer.alloc(MintLayout.span);
  MintLayout.encode({ mintAuthorityOption: 1, mintAuthority: ctx.payer.publicKey, supply: 0n, decimals: 6, isInitialized: true, freezeAuthorityOption: 1, freezeAuthority: ctx.payer.publicKey }, data);
  ctx.svm.setAccount(USDC, { lamports: 1_000_000_000, data, owner: TOKEN_PROGRAM_ID, executable: false });
}

/** The protocol's three USDC accounts (burn, treasury, stakers' rewards). */
export async function usdcSides(ctx: Ctx) {
  return {
    burn: await createAta(ctx, USDC, pda.burn()),
    treasury: await createAta(ctx, USDC, pda.protocol()),
    rewards: await createAta(ctx, USDC, pda.stakeRewards()),
  };
}

type Sides = Awaited<ReturnType<typeof usdcSides>>;

/** The fee accounts a flow passes after its quads. */
export function feeBlock(sides: Sides, payer: PublicKey, cut: PublicKey = sides.treasury, payerUsdc: PublicKey = ata(USDC, payer)) {
  const m = (pubkey: PublicKey, isWritable = true) => ({ pubkey, isSigner: false, isWritable });
  return [m(SYSVAR_INSTRUCTIONS_PUBKEY, false), m(payerUsdc), m(sides.burn), m(sides.treasury), m(sides.rewards), m(cut), m(TOKEN_PROGRAM_ID, false)];
}

export const quoteIx = (ctx: Ctx, key: Keypair, value: bigint): Promise<TransactionInstruction> =>
  ctx.vault.methods.feeQuote(new BN(value.toString())).accountsPartial({ priceAuthority: key.publicKey, keepers: pda.keepers() }).instruction();

