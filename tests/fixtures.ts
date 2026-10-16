// Builds a ready-to-use world: protocol initialised, one classic-SPL and one Token-2022
// asset listed, and a seeded two-asset basket. Mixed token programs are the default on
// purpose, since mixed baskets are the product.
import { Keypair, PublicKey } from "@solana/web3.js";
import {
  BN,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  ata,
  createAta,
  createMint,
  fakeProgramData,
  fund,
  mintTo,
  pda,
  rpc,
  setup,
  type Ctx,
} from "./helpers.ts";

export const UNITS_A = 2_000_000n; // 2.0 of token A (6 decimals) per share
export const UNITS_B = 500_000_000n; // 0.5 of token B (9 decimals) per share
export const SEED_SHARES = 10n * 1_000_000_000n; // 10 whole shares

export type Access = "public" | "allowlist" | "private";
export type Control = "creator" | "dao" | "immutable";

export interface WorldOpts {
  access?: Access;
  control?: Control;
  creatorFeeBps?: number;
  protocolFeeBps?: number;
  burnShareBps?: number;
  seed?: boolean;
  /** Token program for the share mint: Token-2022 gives it a scaled UI amount (splits). */
  shareTokenProgram?: PublicKey;
  /** Base units of token A per share at seeding; small values behave like wBTC (8 decimals,
   *  high price), where one unit is a large fraction of a share's slice. */
  unitsA?: bigint;
  /** A Token-2022 transfer fee on token B, in bps (as stonkfun tokens and PreStocks charge). */
  feeBpsB?: number;
}

export async function listAsset(ctx: Ctx, mint: PublicKey) {
  await rpc(
    ctx,
    ctx.vault.methods.registerAsset().accountsPartial({
      payer: ctx.payer.publicKey,
      authority: ctx.payer.publicKey,
      protocol: pda.protocol(),
      mint,
      assetEntry: pda.asset(mint),
    }),
  );
}

export async function initProtocol(ctx: Ctx, protocolFeeBps = 10, burnShareBps = 5_000) {
  const graviMint = await createMint(ctx, 9);
  fakeProgramData(ctx, ctx.payer.publicKey);
  await rpc(
    ctx,
    ctx.vault.methods.initProtocol(graviMint, protocolFeeBps, burnShareBps).accountsPartial({
      authority: ctx.payer.publicKey,
      protocol: pda.protocol(),
      programData: PublicKey.findProgramAddressSync(
        [ctx.vault.programId.toBuffer()],
        new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111"),
      )[0],
    }),
  );
  return graviMint;
}

export async function world(opts: WorldOpts = {}) {
  const ctx = setup();
  // No mint/redeem fee unless a test asks for one, so share arithmetic elsewhere stays exact.
  const graviMint = await initProtocol(ctx, opts.protocolFeeBps ?? 0, opts.burnShareBps ?? 5_000);

  const mintA = await createMint(ctx, 6, TOKEN_PROGRAM_ID);
  const mintB = await createMint(ctx, 9, TOKEN_2022_PROGRAM_ID, undefined, opts.feeBpsB);
  await listAsset(ctx, mintA);
  await listAsset(ctx, mintB);

  const creator = fund(ctx);
  const basketId = new Uint8Array(16);
  basketId[0] = 7;
  const basket = pda.basket(creator.publicKey, basketId);
  const shareMint = pda.share(basket);
  const shareProgram = opts.shareTokenProgram ?? TOKEN_PROGRAM_ID;

  await rpc(
    ctx,
    ctx.vault.methods
      .initializeBasket({
        basketId: Array.from(basketId),
        access: { [opts.access ?? "public"]: {} },
        control: { [opts.control ?? "creator"]: {} },
        rebalanceMode: { hybrid: {} },
        intervalSecs: new BN(30 * 86_400),
        driftBps: 500,
        mgmtFeeBps: opts.creatorFeeBps ?? 20,
        mints: [mintA, mintB],
        targetBps: [6_000, 4_000],
      })
      .accountsPartial({
        creator: creator.publicKey,
        basket,
        shareMint,
        shareTokenProgram: shareProgram,
      })
      .remainingAccounts([
        { pubkey: mintA, isSigner: false, isWritable: false },
        { pubkey: pda.asset(mintA), isSigner: false, isWritable: false },
        { pubkey: mintB, isSigner: false, isWritable: false },
        { pubkey: pda.asset(mintB), isSigner: false, isWritable: false },
      ]),
    [creator],
  );

  await createFeeAccounts(ctx, shareMint, shareProgram);
  const vaultA = await createAta(ctx, mintA, basket, TOKEN_PROGRAM_ID);
  const vaultB = await createAta(ctx, mintB, basket, TOKEN_2022_PROGRAM_ID);

  const w = { ctx, graviMint, mintA, mintB, creator, basket, basketId, shareMint, vaultA, vaultB, shareProgram };

  const holder = await fundHolder(w, creator);
  if (opts.seed ?? true) {
    await rpc(
      ctx,
      ctx.vault.methods
        .seedBasket(new BN(SEED_SHARES.toString()), [new BN((opts.unitsA ?? UNITS_A).toString()), new BN(UNITS_B.toString())])
        .accountsPartial({
          payer: creator.publicKey,
          basket,
          shareMint,
          payerShares: holder.shares,
          ...feeShares(shareMint, shareProgram),
          allowEntry: null,
          shareTokenProgram: shareProgram,
        })
        .remainingAccounts(quads(w, creator.publicKey)),
      [creator],
    );
  }
  return w;
}

export type World = Awaited<ReturnType<typeof world>>;

/** Where the protocol's mint/redeem fee lands: the burn PDA's and the treasury's share accounts. */
export function feeShares(shareMint: PublicKey, shareProgram = TOKEN_PROGRAM_ID) {
  return { burnShares: ata(shareMint, pda.burn(), shareProgram), treasuryShares: ata(shareMint, pda.protocol(), shareProgram) };
}

export async function createFeeAccounts(ctx: Ctx, shareMint: PublicKey, shareProgram = TOKEN_PROGRAM_ID) {
  await createAta(ctx, shareMint, pda.burn(), shareProgram);
  await createAta(ctx, shareMint, pda.protocol(), shareProgram);
  return feeShares(shareMint, shareProgram);
}

/** Gives `who` token accounts for both constituents (well funded) and for shares. */
export async function fundHolder(w: Pick<World, "ctx" | "mintA" | "mintB" | "shareMint"> & { shareProgram?: PublicKey }, who: Keypair) {
  const a = await createAta(w.ctx, w.mintA, who.publicKey, TOKEN_PROGRAM_ID);
  const b = await createAta(w.ctx, w.mintB, who.publicKey, TOKEN_2022_PROGRAM_ID);
  await mintTo(w.ctx, w.mintA, a, 1_000_000_000_000n, TOKEN_PROGRAM_ID);
  await mintTo(w.ctx, w.mintB, b, 1_000_000_000_000_000n, TOKEN_2022_PROGRAM_ID);
  const shares = await createAta(w.ctx, w.shareMint, who.publicKey, w.shareProgram ?? TOKEN_PROGRAM_ID);
  return { a, b, shares };
}

/** `[vault_ata, user_ata, mint, token_program]` per constituent, as mint/redeem expect. */
export function quads(w: Pick<World, "mintA" | "mintB" | "vaultA" | "vaultB">, user: PublicKey) {
  const row = (vaultAta: PublicKey, userAta: PublicKey, mint: PublicKey, program: PublicKey) => [
    { pubkey: vaultAta, isSigner: false, isWritable: true },
    { pubkey: userAta, isSigner: false, isWritable: true },
    { pubkey: mint, isSigner: false, isWritable: false },
    { pubkey: program, isSigner: false, isWritable: false },
  ];
  return [
    ...row(w.vaultA, ata(w.mintA, user, TOKEN_PROGRAM_ID), w.mintA, TOKEN_PROGRAM_ID),
    ...row(w.vaultB, ata(w.mintB, user, TOKEN_2022_PROGRAM_ID), w.mintB, TOKEN_2022_PROGRAM_ID),
  ];
}

export function mintIx(w: World, who: Keypair, shares: bigint, allowEntry: PublicKey | null = null, minShares?: bigint) {
  return w.ctx.vault.methods
    .mintInKind(new BN(shares.toString()), new BN((minShares ?? shares).toString()))
    .accountsPartial({
      payer: who.publicKey,
      basket: w.basket,
      shareMint: w.shareMint,
      payerShares: ata(w.shareMint, who.publicKey, w.shareProgram),
      ...feeShares(w.shareMint, w.shareProgram),
      allowEntry,
      shareTokenProgram: w.shareProgram,
    })
    .remainingAccounts(quads(w, who.publicKey));
}

export function redeemIx(w: World, who: Keypair, shares: bigint, forfeitMask = 0) {
  return w.ctx.vault.methods
    .redeemInKind(new BN(shares.toString()), forfeitMask)
    .accountsPartial({
      payer: who.publicKey,
      basket: w.basket,
      shareMint: w.shareMint,
      payerShares: ata(w.shareMint, who.publicKey, w.shareProgram),
      ...feeShares(w.shareMint, w.shareProgram),
      shareTokenProgram: w.shareProgram,
    })
    .remainingAccounts(quads(w, who.publicKey));
}
