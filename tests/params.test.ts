// The configurable release: the protocol's tunables in a params account (`set_param`, each within
// bounds written in the program, user-facing ones after a week's notice), kill switches, the merged
// admin setter (`set_role`) and the per-basket extension account. With the params account missing,
// or a value unset, everything reads as the constant it replaced: the rest of the suite runs that way.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { Keypair, PublicKey } from "@solana/web3.js";
import { BN, TOKEN_PROGRAM_ID, ata, expectError, fund, pda, rpc, setup, warp, now } from "./helpers.ts";
import {
  CONFIG,
  FAMILY,
  NO_GRAVI,
  PARAM,
  PARAMS,
  ROLE,
  SEED_SHARES,
  UNITS_A,
  UNITS_B,
  feeShares,
  fundHolder,
  paramSlot,
  quads,
  initKeepers,
  initProtocol,
  mintIx,
  redeemIx,
  setParam,
  setParamIx,
  setRole,
  setRoleIx,
  world,
  type World,
} from "./fixtures.ts";

const DAY = 86_400;
const NOTICE = 7 * DAY;
const SHARE = 1_000_000_000n;

// id: [name, default, min, max, how it applies] — the table in params.rs.
type Rule = "now" | "wait" | "up" | "down";
const SPEC: [string, bigint, bigint, bigint, Rule][] = [
  ["KILL", 0n, 0n, 0n, "now"],
  ["FEATURES", 0n, 0n, 0n, "wait"],
  ["REBALANCE_SLIPPAGE_BPS", 300n, 10n, 500n, "up"],
  ["BUYBACK_SLIPPAGE_BPS", 1_000n, 50n, 1_500n, "up"],
  ["SLICE_BPS", 500n, 50n, 1_000n, "up"],
  ["LEAVING_SLICE_BPS", 5_000n, 500n, 10_000n, "up"],
  ["KEEPER_REFUND_E12", 50_000_000_000n, 1n, 250_000_000_000n, "up"],
  ["BUYBACK_REFUND_E12", 500_000_000_000n, 1n, 2_000_000_000_000n, "up"],
  ["PRICE_MOVE_BPS", 2_000n, 200n, 3_000n, "up"],
  ["PRICE_REFERENCE_SECS", 3_600n, 900n, 86_400n, "down"],
  ["PYTH_AGE_SECS", 300n, 30n, 900n, "up"],
  ["PYTH_TOLERANCE_MIN_BPS", 300n, 50n, 1_000n, "up"],
  ["PYTH_TOLERANCE_MAX_BPS", 500n, 100n, 1_000n, "up"],
  ["PYTH_CONF_MULTIPLE", 3n, 1n, 10n, "up"],
  ["PRICE_AGE_SECS", 120n, 30n, 600n, "up"],
  ["DEAD_PRICE_SECS", 86_400n, 3_600n, 30n * 86_400n, "wait"],
  ["STOP_CONFIRM_SECS", 300n, 60n, 3_600n, "wait"],
  ["STOP_WINDOW_SECS", 900n, 120n, 10_800n, "wait"],
  ["STOP_MIN_BPS", 500n, 100n, 2_000n, "wait"],
  ["STOP_MAX_BPS", 9_000n, 5_000n, 9_500n, "wait"],
  ["POLICY_MIN_DRIFT_BPS", 50n, 10n, 500n, "wait"],
  ["POLICY_MIN_INTERVAL_SECS", 86_400n, 3_600n, 7n * 86_400n, "wait"],
  ["TRANSFER_FEE_CAP_BPS", 400n, 100n, 800n, "wait"],
  ["SPLIT_COOLDOWN_SECS", 30n * 86_400n, 86_400n, 90n * 86_400n, "wait"],
  ["CUT_NOTICE_SECS", 86_400n, 6n * 3_600n, 30n * 86_400n, "wait"],
  ["INVITE_MAX_SECS", 31n * 86_400n, 86_400n, 93n * 86_400n, "now"],
  ["SMART_MAX_DAY_BPS", 4_000n, 500n, 6_000n, "wait"],
  ["SMART_MAX_WEEK_BPS", 10_000n, 1_000n, 15_000n, "wait"],
  ["SMART_MAX_WEIGHT_BPS", 3_500n, 1_000n, 6_000n, "wait"],
  ["SMART_MAX_UNVERIFIED_BPS", 3_000n, 100n, 5_000n, "wait"],
  ["MANDATE_NOTICE_SECS", 86_400n, 6n * 3_600n, 14n * 86_400n, "wait"],
  ["STAKE_WARMUP_SECS", 2n * 86_400n, 3_600n, 14n * 86_400n, "wait"],
  ["STAKE_UNSTAKE_SECS", 7n * 86_400n, 86_400n, 30n * 86_400n, "wait"],
  ["STAKE_PERIOD_SECS", 30n * 86_400n, 7n * 86_400n, 90n * 86_400n, "wait"],
  ["VOTE_PERIOD_SECS", 3n * 86_400n, 86_400n, 14n * 86_400n, "wait"],
  ["VOTE_TIMELOCK_SECS", 86_400n, 3_600n, 7n * 86_400n, "wait"],
  ["VOTE_GRACE_SECS", 7n * 86_400n, 86_400n, 30n * 86_400n, "wait"],
  ["PROPOSE_MIN_BPS", 100n, 10n, 1_000n, "wait"],
  ["QUORUM_BPS", 1_000n, 100n, 5_000n, "wait"],
  ["SUPERMAJORITY_BPS", 6_600n, 5_100n, 9_000n, "wait"],
  ["MAX_OPEN_PROPOSALS", 3n, 1n, 10n, "wait"],
];
const P = PARAM;
assert.deepEqual(SPEC.map(([name]) => name), Object.keys(PARAM), "the table here and PARAM in fixtures.ts list the same ids");
const slot = paramSlot;
/** The lister's key, read from its account (`[discriminator][bump][key]`). */
const listerOf = (ctx: ReturnType<typeof setup>) => new PublicKey(Buffer.from(ctx.svm.getAccount(pda.lister())!.data).subarray(9, 41));

async function protocolOnly() {
  const ctx = setup();
  warp(ctx, 1_000_000);
  const graviMint = await initProtocol(ctx, 0);
  return { ctx, graviMint };
}

describe("params: the account", () => {
  it("sits at [params], the address every instruction pins it to", async () => {
    const idl = (await import("../target/idl/gravifi_vault.json", { with: { type: "json" } })).default as any;
    const pinned = new Set(idl.instructions.flatMap((ix: any) => ix.accounts.filter((a: any) => a.name === "params").map((a: any) => a.address)));
    assert.deepEqual([...pinned], [PARAMS.toBase58()]);
    assert.equal(PARAMS.toBase58(), "5atmoVAHht1bK7MsEogYY2e8xZgYMGT9G2nVBPeTebr1");
  });

  it("is created by the first set_param, the authority paying, with its version byte", async () => {
    const { ctx } = await protocolOnly();
    assert.equal(ctx.svm.getAccount(PARAMS), null);
    await setParam(ctx, P.INVITE_MAX_SECS, 10n * BigInt(DAY));
    const acct = ctx.svm.getAccount(PARAMS)!;
    assert.ok(acct.owner.equals(ctx.vault.programId));
    assert.equal(acct.data.length, 16 + 80 * 24);
    assert.equal(acct.data[8], 1, "version");
    assert.equal(acct.data[9], 255, "bump");
  });

  it("only the protocol authority sets a param, and only a known one", async () => {
    const { ctx } = await protocolOnly();
    const outsider = fund(ctx);
    await expectError(rpc(ctx, setParamIx(ctx, P.PRICE_AGE_SECS, 60n, [], outsider.publicKey), [outsider]), "NotProtocolAuthority");
    await expectError(setParam(ctx, SPEC.length, 1n), "UnknownParam");
    await expectError(setParam(ctx, 999, 1n), "UnknownParam");
    await expectError(setParam(ctx, 1_006, 1n), "UnknownParam");
  });

  it("every param: bounds hold, and each applies at once or after the week's notice as its rule says", async () => {
    const { ctx } = await protocolOnly();
    for (const [name, def, min, max, rule] of SPEC.slice(2)) {
      const id = P[name];
      if (min > 1n) await expectError(setParam(ctx, id, min - 1n), "ParamOutOfBounds");
      await expectError(setParam(ctx, id, max + 1n), "ParamOutOfBounds");
      const t = BigInt(now(ctx));

      // The lowest value: below the default, so at once unless every change waits.
      await setParam(ctx, id, min);
      let s = slot(ctx, id);
      if (rule === "wait" || rule === "down") assert.deepEqual(s, { value: 0n, pending: min, at: t + BigInt(NOTICE) }, name);
      else assert.deepEqual(s, { value: min, pending: 0n, at: 0n }, name);

      // The highest: a raise, so at once only for operational values and `down` rules.
      await setParam(ctx, id, max);
      s = slot(ctx, id);
      if (rule === "now" || rule === "down") assert.deepEqual(s, { value: max, pending: 0n, at: 0n }, name);
      else assert.deepEqual(s, { value: rule === "up" ? min : 0n, pending: max, at: t + BigInt(NOTICE) }, name);

      // Back to the default (0). A waiting rule's pending raise is withdrawn by proposing the
      // value in force; an `up` rule's return from the minimum is itself a raise, so it waits, and
      // a `down` rule's return from the maximum is a lowering, so it waits too.
      await setParam(ctx, id, 0n);
      s = slot(ctx, id);
      if (rule === "up") assert.deepEqual(s, { value: min, pending: 0n, at: t + BigInt(NOTICE) }, name);
      else if (rule === "down") assert.deepEqual(s, { value: max, pending: 0n, at: t + BigInt(NOTICE) }, name);
      else assert.deepEqual(s, { value: 0n, pending: 0n, at: 0n }, name);
      assert.ok(def > 0n);
    }
  });

  it("a pending value applies once its notice has run out, and folds into the value on the next change", async () => {
    const { ctx } = await protocolOnly();
    await setParam(ctx, P.QUORUM_BPS, 2_000n);
    const at = slot(ctx, P.QUORUM_BPS).at;
    warp(ctx, NOTICE);
    assert.equal(BigInt(now(ctx)) >= at, true);
    // A second change: the matured 20% is the value now, the new 30% waits its own week.
    await setParam(ctx, P.QUORUM_BPS, 3_000n);
    assert.deepEqual(slot(ctx, P.QUORUM_BPS), { value: 2_000n, pending: 3_000n, at: BigInt(now(ctx) + NOTICE) });
  });

  it("kill switches apply at once both ways; unknown bits and dark features this build doesn't know are refused", async () => {
    const { ctx } = await protocolOnly();
    await setParam(ctx, P.KILL, BigInt(FAMILY.mint | FAMILY.create));
    assert.deepEqual(slot(ctx, P.KILL), { value: BigInt(FAMILY.mint | FAMILY.create), pending: 0n, at: 0n });
    await setParam(ctx, P.KILL, 0n);
    assert.equal(slot(ctx, P.KILL).value, 0n);
    await expectError(setParam(ctx, P.KILL, 1n << 10n), "ParamOutOfBounds");
    await expectError(setParam(ctx, P.FEATURES, 1n), "ParamOutOfBounds");
  });
});

describe("params: effects", () => {
  it("defaults: with the account created and values unset, a basket mints and redeems as before", async () => {
    const w = await world();
    await setParam(w.ctx, P.INVITE_MAX_SECS, 5n * BigInt(DAY));
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await rpc(w.ctx, mintIx(w, alice, 3n * SHARE), [alice]);
    await rpc(w.ctx, redeemIx(w, alice, SHARE), [alice]);
  });

  it("the mint switch stops mints and seeds at once, never a redeem; lifted, mints resume", async () => {
    const w = await world();
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await rpc(w.ctx, mintIx(w, alice, 3n * SHARE), [alice]);
    await setParam(w.ctx, P.KILL, BigInt(FAMILY.mint));
    await expectError(rpc(w.ctx, mintIx(w, alice, SHARE), [alice]), "FeatureOff");
    await rpc(w.ctx, redeemIx(w, alice, SHARE), [alice]);
    await rpc(w.ctx, redeemIx(w, w.creator, SHARE), [w.creator]);
    // A fresh basket can't be opened with its seed either.
    const fresh = await world({ seed: false });
    await setParam(fresh.ctx, P.KILL, BigInt(FAMILY.mint));
    await expectError(rpc(fresh.ctx, seedIx(fresh), [fresh.creator]), "FeatureOff");
    await setParam(w.ctx, P.KILL, 0n);
    await rpc(w.ctx, mintIx(w, alice, SHARE), [alice]);
  });

  it("the create switch stops new baskets", async () => {
    const w = await world();
    await setParam(w.ctx, P.KILL, BigInt(FAMILY.create));
    const maker = fund(w.ctx);
    await expectError(rpc(w.ctx, createIx(w, maker), [maker]), "FeatureOff");
    await setParam(w.ctx, P.KILL, 0n);
    await rpc(w.ctx, createIx(w, maker), [maker]);
  });

  it("a price bound tightens at once and loosens only after the week's notice: the band without Pyth", async () => {
    const w = await world();
    const keeper = fund(w.ctx);
    await initKeepers(w.ctx, keeper.publicKey, keeper.publicKey);
    const post = (price: bigint) =>
      rpc(w.ctx, w.ctx.vault.methods.setPrice(new BN(price.toString())).accountsPartial({ priceAuthority: keeper.publicKey, keepers: pda.keepers(), mint: w.mintA, assetPrice: pda.price(w.mintA), pythPrice: null }), [keeper]);
    const E12 = 1_000_000_000_000n;
    // The first price seeds the band; a quarter of an hour later it's confirmed.
    await post(E12);
    warp(w.ctx, 900);
    await post(E12);
    await setParam(w.ctx, P.PRICE_MOVE_BPS, 1_000n);
    await expectError(post((E12 * 115n) / 100n), "PriceMoveTooLarge");
    await post((E12 * 109n) / 100n);
    await setParam(w.ctx, P.PRICE_MOVE_BPS, 0n);
    await expectError(post((E12 * 115n) / 100n), "PriceMoveTooLarge");
    warp(w.ctx, NOTICE);
    await post(E12);
    await post((E12 * 119n) / 100n);
  });

  it("a user-facing value waits its week: the transfer-fee cap on new money", async () => {
    const w = await world({ feeBpsB: 300 });
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await rpc(w.ctx, mintIx(w, alice, 3n * SHARE), [alice]);
    await setParam(w.ctx, P.TRANSFER_FEE_CAP_BPS, 200n);
    await rpc(w.ctx, mintIx(w, alice, SHARE), [alice]);
    warp(w.ctx, NOTICE);
    await expectError(rpc(w.ctx, mintIx(w, alice, SHARE), [alice]), "TransferFeeTooHigh");
    await rpc(w.ctx, redeemIx(w, alice, SHARE), [alice]);
  });
});

describe("set_role", () => {
  it("names every role, creating the lister's, strategist's and keepers' accounts the first time", async () => {
    const ctx = setup();
    await initProtocol(ctx, 0);
    const k = () => Keypair.generate().publicKey;
    const guardian = k(), lister = k(), strategist = k(), price = k(), rebalancer = k(), exempt = k();
    await setRole(ctx, ROLE.guardian, guardian);
    await setRole(ctx, ROLE.lister, lister);
    await setRole(ctx, ROLE.strategist, strategist);
    await setRole(ctx, ROLE.priceAuthority, price);
    await setRole(ctx, ROLE.rebalancer, rebalancer);
    await setRole(ctx, ROLE.burnExempt, exempt);
    const p = await ctx.vault.account.protocol.fetch(pda.protocol());
    assert.ok(p.guardian.equals(guardian));
    assert.ok(listerOf(ctx).equals(lister));
    assert.ok((await ctx.vault.account.strategist.fetch(pda.strategist())).strategist.equals(strategist));
    const keepers = await ctx.vault.account.keepers.fetch(pda.keepers());
    assert.ok(keepers.priceAuthority.equals(price));
    assert.ok(keepers.rebalancer.equals(rebalancer));
    const raw = Buffer.from(ctx.svm.getAccount(pda.protocol())!.data);
    assert.ok(new PublicKey(raw.subarray(113, 145)).equals(exempt));
    // Replacing one keeper leaves the other.
    const price2 = k();
    await setRole(ctx, ROLE.priceAuthority, price2);
    const after = await ctx.vault.account.keepers.fetch(pda.keepers());
    assert.ok(after.priceAuthority.equals(price2) && after.rebalancer.equals(rebalancer));
    // Removing: the default key.
    await setRole(ctx, ROLE.lister, PublicKey.default);
    assert.ok(listerOf(ctx).equals(PublicKey.default));
  });

  it("hands the protocol on; the old authority can do nothing after, and the authority can't be emptied", async () => {
    const ctx = setup();
    await initProtocol(ctx, 0);
    await expectError(setRole(ctx, ROLE.authority, PublicKey.default), "BadParameter");
    const next = fund(ctx);
    await setRole(ctx, ROLE.authority, next.publicKey);
    await expectError(setRole(ctx, ROLE.guardian, PublicKey.unique()), "NotProtocolAuthority");
    await setRole(ctx, ROLE.guardian, PublicKey.unique(), next);
    await setParam(ctx, P.PRICE_AGE_SECS, 60n, [], next);
  });

  it("refuses an outsider, an unknown role, a wrong role account, and a second GRAVI mint", async () => {
    const ctx = setup();
    const gravi = await initProtocol(ctx, 0);
    const outsider = fund(ctx);
    await expectError(rpc(ctx, setRoleIx(ctx, ROLE.guardian, outsider.publicKey, outsider.publicKey), [outsider]), "NotProtocolAuthority");
    await expectError(setRole(ctx, 8, PublicKey.unique()), "BadRole");
    const wrong = ctx.vault.methods
      .setRole(ROLE.lister, PublicKey.unique())
      .accountsPartial({ authority: ctx.payer.publicKey, protocol: pda.protocol() })
      .remainingAccounts([{ pubkey: pda.strategist(), isWritable: true, isSigner: false }]);
    await expectError(rpc(ctx, wrong), "BadRole");
    const none = ctx.vault.methods.setRole(ROLE.lister, PublicKey.unique()).accountsPartial({ authority: ctx.payer.publicKey, protocol: pda.protocol() });
    await expectError(rpc(ctx, none), "BadRole");
    assert.ok(gravi);
    await expectError(setRole(ctx, ROLE.graviMint, PublicKey.unique()), "GraviMintAlreadySet");
  });

  it("the other settings: fees, creation and smart burns, smart fee, Community's door, each within its code ceiling", async () => {
    const w = await world();
    const { ctx, graviMint } = w;
    await setParam(ctx, CONFIG.protocolFees, 40n | (6_000n << 16n));
    let p = await ctx.vault.account.protocol.fetch(pda.protocol());
    assert.equal(p.protocolFeeBps, 40);
    assert.equal(p.burnShareBps, 6_000);
    await expectError(setParam(ctx, CONFIG.protocolFees, 51n | (5_000n << 16n)), "FeeTooHigh");
    await expectError(setParam(ctx, CONFIG.protocolFees, 40n | (4_999n << 16n)), "BurnShareTooLow");
    await expectError(setParam(ctx, CONFIG.smartFee, 101n), "FeeTooHigh");
    await setParam(ctx, CONFIG.smartFee, 90n);
    await expectError(setParam(ctx, CONFIG.creationBurn, 1_000_001n * 1_000_000_000n, [pda.creationBurn(), graviMint]), "CreationBurnTooHigh");
    await setParam(ctx, CONFIG.creationBurn, 7n, [pda.creationBurn(), graviMint]);
    assert.equal((await ctx.vault.account.creationBurn.fetch(pda.creationBurn())).perAsset.toString(), "7");
    await expectError(setParam(ctx, CONFIG.smartBurn, 5n, [PublicKey.unique()]), "GraviMintNotSet");
    await setParam(ctx, CONFIG.smartBurn, 5n, [graviMint]);
    await setParam(ctx, CONFIG.communityOpen, 1n);
    await expectError(setParam(ctx, CONFIG.communityOpen, 2n), "BadParameter");
    const raw = Buffer.from(ctx.svm.getAccount(pda.protocol())!.data);
    assert.equal(raw.readUInt16LE(145), 90);
    assert.equal(raw[147], 1);
    assert.equal(raw.readBigUInt64LE(148), 5n);
    assert.equal(raw[156] & 2, 2);
    p = await ctx.vault.account.protocol.fetch(pda.protocol());
    assert.equal(p.protocolFeeBps, 40);
  });
});

describe("the basket extension account", () => {
  const extOf = (basket: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("ext"), basket.toBuffer()], new PublicKey("HHmD4ZP5he9GrcmcNrpjahyK1dWh2MhMwmVpiPRJsw3G"))[0];
  const init = (w: World, payer: Keypair, ext = extOf(w.basket)) =>
    rpc(w.ctx, w.ctx.vault.methods.extInit().accountsPartial({ payer: payer.publicKey, basket: w.basket, ext }), [payer]);

  it("is created by whoever first needs it, once, empty, at [ext, basket]", async () => {
    const w = await world();
    const payer = fund(w.ctx);
    const before = w.ctx.svm.getBalance(payer.publicKey)!;
    await init(w, payer);
    const acct = w.ctx.svm.getAccount(extOf(w.basket))!;
    assert.ok(acct.owner.equals(w.ctx.vault.programId));
    assert.equal(acct.data.length, 300);
    const d = Buffer.from(acct.data);
    assert.equal(d[8], 1, "version");
    assert.ok(new PublicKey(d.subarray(10, 42)).equals(w.basket));
    assert.equal(d.readUInt16LE(42), 0, "no records yet");
    assert.ok(w.ctx.svm.getBalance(payer.publicKey)! < before, "the payer paid its rent");
    // Again: nothing changes.
    await init(w, fund(w.ctx));
    assert.deepEqual(Buffer.from(w.ctx.svm.getAccount(extOf(w.basket))!.data), d);
  });

  it("refuses another address", async () => {
    const w = await world();
    const payer = fund(w.ctx);
    await expectError(init(w, payer, extOf(PublicKey.unique())), "BadExt");
  });
});

/** A basket's first deposit, for a world built with `seed: false`. */
function seedIx(w: World) {
  return w.ctx.vault.methods
    .seedBasket(new BN(SEED_SHARES.toString()), [new BN(UNITS_A.toString()), new BN(UNITS_B.toString())])
    .accountsPartial({
      payer: w.creator.publicKey,
      basket: w.basket,
      shareMint: w.shareMint,
      payerShares: ata(w.shareMint, w.creator.publicKey, w.shareProgram),
      ...feeShares(w.shareMint, w.shareProgram),
      shareTokenProgram: w.shareProgram,
    })
    .remainingAccounts(quads(w, w.creator.publicKey));
}

/** A new two-asset basket by `maker`, of the world's listed assets. */
function createIx(w: World, maker: Keypair) {
  const id = Array.from(Keypair.generate().publicKey.toBytes().subarray(0, 16));
  const basket = pda.basket(maker.publicKey, Uint8Array.from(id));
  return w.ctx.vault.methods
    .initializeBasket({
      basketId: id,
      access: { public: {} },
      control: { creator: {} },
      rebalanceMode: { hybrid: {} },
      intervalSecs: new BN(30 * DAY),
      driftBps: 500,
      mgmtFeeBps: 0,
      mints: [w.mintA, w.mintB],
      targetBps: [5_000, 5_000],
    })
    .accountsPartial({ ...NO_GRAVI, creator: maker.publicKey, basket, shareMint: pda.share(basket), shareTokenProgram: TOKEN_PROGRAM_ID })
    .remainingAccounts([w.mintA, pda.asset(w.mintA), w.mintB, pda.asset(w.mintB)].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false })));
}
