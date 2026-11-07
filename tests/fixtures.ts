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
  /** Token B's issuer can freeze accounts and pause the mint, as tokenized stocks' can. */
  issuerB?: { freezable?: boolean; pausable?: boolean };
  /** GRAVI's token program and decimals: mainnet's GRAVI is Token-2022 with 6. */
  gravi?: { program: PublicKey; decimals: number };
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

/** Leaves the GRAVI accounts out of a basket's seed, while the creation burn is off. */
export const NO_GRAVI = { graviMint: null, creatorGravi: null, graviTokenProgram: null };

export async function initProtocol(ctx: Ctx, protocolFeeBps = 10, burnShareBps = 5_000, gravi = { program: TOKEN_PROGRAM_ID, decimals: 9 }) {
  const graviMint = await createMint(ctx, gravi.decimals, gravi.program);
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
  // Every basket's seed names the creation-burn account, so it exists from the start (off).
  await setCreationBurn(ctx, graviMint, 0n);
  return graviMint;
}

/** Sets the GRAVI burned to create a basket, as the protocol authority (the payer). */
export async function setCreationBurn(ctx: Ctx, graviMint: PublicKey, amount: bigint, authority = ctx.payer) {
  await rpc(ctx, setParamIx(ctx, CONFIG.creationBurn, amount, [pda.creationBurn(), graviMint], authority.publicKey), [authority]);
}

// ------------------------------------------------------------------- roles and params

/** `set_role`'s roles (instructions/protocol.rs). */
export const ROLE = { authority: 0, guardian: 1, graviMint: 2, burnExempt: 3, lister: 4, strategist: 5, priceAuthority: 6, rebalancer: 7 } as const;
/** `set_param`'s settings kept outside the params account (instructions/protocol.rs). */
export const CONFIG = { protocolFees: 1_000, creationBurn: 1_001, smartFee: 1_002, smartBurn: 1_003, stakeThreshold: 1_004, communityOpen: 1_005 } as const;
/** `set_param`'s ids for the params account, as in params.rs. */
export const PARAM = Object.fromEntries(
  [
    "KILL", "FEATURES", "REBALANCE_SLIPPAGE_BPS", "BUYBACK_SLIPPAGE_BPS", "SLICE_BPS", "LEAVING_SLICE_BPS", "KEEPER_REFUND_E12",
    "BUYBACK_REFUND_E12", "PRICE_MOVE_BPS", "PRICE_REFERENCE_SECS", "PYTH_AGE_SECS", "PYTH_TOLERANCE_MIN_BPS", "PYTH_TOLERANCE_MAX_BPS",
    "PYTH_CONF_MULTIPLE", "PRICE_AGE_SECS", "DEAD_PRICE_SECS", "STOP_CONFIRM_SECS", "STOP_WINDOW_SECS", "STOP_MIN_BPS", "STOP_MAX_BPS",
    "POLICY_MIN_DRIFT_BPS", "POLICY_MIN_INTERVAL_SECS", "TRANSFER_FEE_CAP_BPS", "SPLIT_COOLDOWN_SECS", "CUT_NOTICE_SECS", "INVITE_MAX_SECS",
    "SMART_MAX_DAY_BPS", "SMART_MAX_WEEK_BPS", "SMART_MAX_WEIGHT_BPS", "SMART_MAX_UNVERIFIED_BPS", "MANDATE_NOTICE_SECS", "STAKE_WARMUP_SECS",
    "STAKE_UNSTAKE_SECS", "STAKE_PERIOD_SECS", "VOTE_PERIOD_SECS", "VOTE_TIMELOCK_SECS", "VOTE_GRACE_SECS", "PROPOSE_MIN_BPS", "QUORUM_BPS",
    "SUPERMAJORITY_BPS", "MAX_OPEN_PROPOSALS",
  ].map((name, id) => [name, id]),
) as Record<string, number>;
/** The kill switches' bits (`KILL`). */
export const FAMILY = { mint: 1, rebalance: 2, buyback: 4, stops: 8, smart: 16, staking: 32, community: 64, invites: 128, listing: 256, create: 512 } as const;
/** The week of notice on user-facing changes. */
export const PARAM_NOTICE = 7 * 86_400;

/** A param's slot in the params account: [value][pending][pending_at]; zeros before it exists. */
export function paramSlot(ctx: Ctx, id: number) {
  const acct = ctx.svm.getAccount(PARAMS);
  if (!acct) return { value: 0n, pending: 0n, at: 0n };
  const d = Buffer.from(acct.data);
  const o = 16 + id * 24;
  return { value: d.readBigUInt64LE(o), pending: d.readBigUInt64LE(o + 8), at: d.readBigInt64LE(o + 16) };
}

/** The params account's address, `[b"params"]`. */
export const PARAMS = PublicKey.findProgramAddressSync([Buffer.from("params")], new PublicKey("HHmD4ZP5he9GrcmcNrpjahyK1dWh2MhMwmVpiPRJsw3G"))[0];

const roleAccount = (role: number) => (role === ROLE.lister ? pda.lister() : role === ROLE.strategist ? pda.strategist() : role >= ROLE.priceAuthority ? pda.keepers() : null);

/** `set_role`, as a builder for `rpc`/`expectError`. */
export function setRoleIx(ctx: Ctx, role: number, key: PublicKey, authority = ctx.payer.publicKey) {
  const acct = roleAccount(role);
  return ctx.vault.methods
    .setRole(role, key)
    .accountsPartial({ authority, protocol: pda.protocol() })
    .remainingAccounts(acct ? [{ pubkey: acct, isWritable: true, isSigner: false }] : []);
}

/** `set_param`, as a builder: `remaining` are writable where the setting writes them. */
export function setParamIx(ctx: Ctx, id: number, value: bigint | number, remaining: PublicKey[] = [], authority = ctx.payer.publicKey) {
  // The creation burn's account and the stake pool are written; the GRAVI mint is only read.
  const writable = (i: number) => (id === CONFIG.creationBurn && i === 0) || id === CONFIG.stakeThreshold;
  return ctx.vault.methods
    .setParam(id, new BN(value.toString()))
    .accountsPartial({ authority, protocol: pda.protocol() })
    .remainingAccounts(remaining.map((pubkey, i) => ({ pubkey, isWritable: writable(i), isSigner: false })));
}

export async function setRole(ctx: Ctx, role: number, key: PublicKey, authority = ctx.payer) {
  await rpc(ctx, setRoleIx(ctx, role, key, authority.publicKey), [authority]);
}

export async function setParam(ctx: Ctx, id: number, value: bigint | number, remaining: PublicKey[] = [], authority = ctx.payer) {
  await rpc(ctx, setParamIx(ctx, id, value, remaining, authority.publicKey), [authority]);
}

/** Names the price authority and the rebalancer (`set_role` twice). */
export async function initKeepers(ctx: Ctx, priceAuthority: PublicKey, rebalancer: PublicKey, authority = ctx.payer) {
  await setRole(ctx, ROLE.priceAuthority, priceAuthority, authority);
  await setRole(ctx, ROLE.rebalancer, rebalancer, authority);
}

/** The protocol fee and the burn's share of it, as one `set_param` value. */
export function protocolFeesIx(ctx: Ctx, feeBps: number, burnShareBps: number, authority = ctx.payer.publicKey) {
  return setParamIx(ctx, CONFIG.protocolFees, BigInt(feeBps) | (BigInt(burnShareBps) << 16n), [], authority);
}

export async function world(opts: WorldOpts = {}) {
  const ctx = setup();
  // No mint/redeem fee unless a test asks for one, so share arithmetic elsewhere stays exact.
  const graviMint = await initProtocol(ctx, opts.protocolFeeBps ?? 0, opts.burnShareBps ?? 5_000, opts.gravi);

  const mintA = await createMint(ctx, 6, TOKEN_PROGRAM_ID);
  const mintB = await createMint(ctx, 9, TOKEN_2022_PROGRAM_ID, undefined, opts.feeBpsB, opts.issuerB);
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
        ...NO_GRAVI,
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
