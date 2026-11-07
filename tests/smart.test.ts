// Smart baskets: the strategist (the app's key) sets a smart basket's targets and assets, only
// within the guardrails its creator wrote: change per 24 hours and per 7 days, weight per asset
// (less without a Verified record), how many assets, a mandate hash changed only after a day's
// notice, and a pause. Mints and redeems pay the Smart-mode fee; the strategist's adds burn no GRAVI.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import type { Keypair } from "@solana/web3.js";
import {
  BN,
  PublicKey,
  TOKEN_PROGRAM_ID,
  ata,
  balance,
  createAta,
  createMint,
  expectError,
  fund,
  mintTo,
  now,
  pda,
  postPrices,
  rpc,
  setup,
  supply,
  warp,
  type Ctx,
} from "./helpers.ts";
import { NO_GRAVI, SEED_SHARES, UNITS_A, UNITS_B, feeShares, fundHolder, initProtocol, listAsset, mintIx, quads, redeemIx, setCreationBurn, world, initKeepers, protocolFeesIx, setRoleIx, setParamIx, ROLE, CONFIG, setParam, PARAM, PARAM_NOTICE, FAMILY, skipNotices } from "./fixtures.ts";

const HOUR = 3_600;
const DAY = 86_400;
const E12 = 1_000_000_000_000n;

const hash = (s: string) => Array.from(createHash("sha256").update(s).digest());
const MANDATE = hash('{"goal":"momentum"}');

type Guard = {
  maxDayBps: number;
  maxWeekBps: number;
  maxWeightBps: number;
  maxUnverifiedBps: number;
  minAssets: number;
  maxAssets: number;
  stopBps: number;
  stopTrailing: boolean;
};
/** The Smart fee's [u16][1 once set] in the protocol account's tail (after the burn-exempt
 *  wallet), the Smart burn's u64 right after it. */
const SMART_FEE_AT = 145;
const GUARD: Guard = { maxDayBps: 2_000, maxWeekBps: 5_000, maxWeightBps: 3_500, maxUnverifiedBps: 3_000, minAssets: 3, maxAssets: 6, stopBps: 3_500, stopTrailing: true };

async function setSmartConfig(ctx: Ctx, strategist: PublicKey, feeBps: number, authority = ctx.payer) {
  await rpc(ctx, setRoleIx(ctx, ROLE.strategist, strategist, authority.publicKey), [authority]); skipNotices(ctx);
  await rpc(ctx, setParamIx(ctx, CONFIG.smartFee, feeBps, [], authority.publicKey), [authority]); skipNotices(ctx);
}

async function setVerified(ctx: Ctx, mint: PublicKey, verified: boolean, authority = ctx.payer) {
  await rpc(
    ctx,
    ctx.vault.methods.setVerified(verified).accountsPartial({ authority: authority.publicKey, protocol: pda.protocol(), mint, verified: pda.verified(mint) }),
    [authority],
  );
}

type Gravi = { mint: PublicKey; account: PublicKey };

/** enable_smart, naming the creator's GRAVI accounts given (null leaves them out) and the Verified
 *  records of `verified` mints. */
function enableSmart(ctx: Ctx, basket: PublicKey, creator: Keypair, guard: Guard = GUARD, mandate = MANDATE, gravi: Gravi | null = null, verified: PublicKey[] = []) {
  const accounts = gravi ? { graviMint: gravi.mint, creatorGravi: gravi.account, graviTokenProgram: TOKEN_PROGRAM_ID } : NO_GRAVI;
  return rpc(
    ctx,
    ctx.vault.methods
      .enableSmart(mandate, guard)
      .accountsPartial({ creator: creator.publicKey, basket, smart: pda.smart(basket), protocol: pda.protocol(), creationBurn: pda.creationBurn(), ...accounts })
      .remainingAccounts(verified.map((m) => ({ pubkey: pda.verified(m), isSigner: false, isWritable: false }))),
    [creator],
  );
}

/** Makes a basket smart without enable_smart, as one made smart before it checked the basket's
 *  targets (a two-asset basket can't be within a 35% cap): the control byte, at offset 122. */
function forceSmart(ctx: Ctx, basket: PublicKey) {
  const acc = ctx.svm.getAccount(basket)!;
  const data = Buffer.from(acc.data);
  data[122] = 3;
  ctx.svm.setAccount(basket, { ...acc, data });
}

async function setSmartBurn(ctx: Ctx, graviMint: PublicKey, amount: bigint, authority = ctx.payer) {
  await rpc(ctx, setParamIx(ctx, CONFIG.smartBurn, amount, [graviMint], authority.publicKey), [authority]); skipNotices(ctx);
}

/** Four listed assets at 25% each, A and B Verified, C and D not, and a fifth (E) listed with the
 *  basket's vault ready, for adding. The basket is creator-managed and not seeded. */
async function fourAssets(targetBps = [2_500, 2_500, 2_500, 2_500]) {
  const ctx = setup();
  const graviMint = await initProtocol(ctx, 40);
  const mints: PublicKey[] = [];
  for (let i = 0; i < 5; i++) {
    const m = await createMint(ctx, 6, TOKEN_PROGRAM_ID);
    await listAsset(ctx, m);
    mints.push(m);
  }
  const [A, B, C, D, E] = mints;
  const creator = fund(ctx);
  const strategist = fund(ctx);
  const guardian = fund(ctx);
  await rpc(ctx, setRoleIx(ctx, ROLE.guardian, guardian.publicKey, ctx.payer.publicKey)); skipNotices(ctx);
  await setSmartConfig(ctx, strategist.publicKey, 80);
  await setVerified(ctx, A, true);
  await setVerified(ctx, B, true);

  const basketId = new Uint8Array(16);
  basketId[0] = 9;
  const basket = pda.basket(creator.publicKey, basketId);
  await rpc(
    ctx,
    ctx.vault.methods
      .initializeBasket({
        basketId: Array.from(basketId),
        access: { public: {} },
        control: { creator: {} },
        rebalanceMode: { drift: {} },
        intervalSecs: new BN(0),
        driftBps: 500,
        mgmtFeeBps: 0,
        mints: [A, B, C, D],
        targetBps,
      })
      .accountsPartial({ ...NO_GRAVI, creator: creator.publicKey, basket, shareMint: pda.share(basket), shareTokenProgram: TOKEN_PROGRAM_ID })
      .remainingAccounts([A, B, C, D].flatMap((m) => [m, pda.asset(m)].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false })))),
    [creator],
  );
  for (const m of mints) await createAta(ctx, m, basket, TOKEN_PROGRAM_ID);
  return { ctx, graviMint, A, B, C, D, E, creator, strategist, guardian, basket };
}

/** `fourAssets`, made smart. */
async function smartWorld(guard: Guard = GUARD) {
  const w = await fourAssets();
  await enableSmart(w.ctx, w.basket, w.creator, guard);
  return w;
}
type SW = Awaited<ReturnType<typeof smartWorld>>;

const basketMints = async (w: { ctx: Ctx; basket: PublicKey }) =>
  ((await w.ctx.vault.account.basket.fetch(w.basket)).constituents as { mint: PublicKey }[]).map((c) => c.mint);
const targets = async (w: { ctx: Ctx; basket: PublicKey }) =>
  ((await w.ctx.vault.account.basket.fetch(w.basket)).constituents as { targetBps: number }[]).map((c) => c.targetBps);
const smartAccount = (w: SW) => w.ctx.vault.account.smart.fetch(pda.smart(w.basket)) as Promise<any>;

/** The strategist's set_targets, each constituent's Verified record passed (or `records`). */
async function setTargets(w: SW, bps: number[], signer: Keypair = w.strategist, records?: PublicKey[]) {
  const recs = records ?? (await basketMints(w)).map((m) => pda.verified(m));
  return rpc(
    w.ctx,
    w.ctx.vault.methods
      .smartSetTargets(bps)
      .accountsPartial({ strategist: signer.publicKey, strategistConfig: pda.strategist(), basket: w.basket, smart: pda.smart(w.basket) })
      .remainingAccounts(recs.map((pubkey) => ({ pubkey, isSigner: false, isWritable: false }))),
    [signer],
  );
}

const addAsset = (w: SW, mint: PublicKey, signer: Keypair = w.strategist) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.smartAddConstituent().accountsPartial({
      strategist: signer.publicKey,
      strategistConfig: pda.strategist(),
      basket: w.basket,
      smart: pda.smart(w.basket),
      mint,
      assetEntry: pda.asset(mint),
      vault: ata(mint, w.basket),
    }),
    [signer],
  );

const removeAsset = (w: SW, index: number, signer: Keypair = w.strategist) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.smartRemoveConstituent(index).accountsPartial({ strategist: signer.publicKey, strategistConfig: pda.strategist(), basket: w.basket, smart: pda.smart(w.basket) }),
    [signer],
  );

const pause = (w: SW, paused: boolean, signer: Keypair) =>
  rpc(w.ctx, w.ctx.vault.methods.setSmartPause(paused).accountsPartial({ signer: signer.publicKey, protocol: pda.protocol(), basket: w.basket, smart: pda.smart(w.basket) }), [signer]);

const propose = (w: SW, mandate: number[], guard: Guard, leave = false, signer: Keypair = w.creator) =>
  rpc(w.ctx, w.ctx.vault.methods.proposeMandate(mandate, guard, leave).accountsPartial({ creator: signer.publicKey, basket: w.basket, smart: pda.smart(w.basket) }), [signer]);

const apply = (w: SW) =>
  rpc(w.ctx, w.ctx.vault.methods.applyMandate().accountsPartial({ basket: w.basket, smart: pda.smart(w.basket), creator: w.creator.publicKey }));

describe("smart baskets: becoming smart", () => {
  it("is the creator's, before the first deposit, with guardrails in range", async () => {
    const w = await fourAssets();
    const stranger = fund(w.ctx);
    await expectError(enableSmart(w.ctx, w.basket, stranger), "NotMandateAuthority");
    const bad: [Partial<Guard>, string][] = [
      [{ maxDayBps: 4_001, maxWeekBps: 5_000 }, "day over 40%"],
      [{ maxDayBps: 0 }, "no change at all"],
      [{ maxWeekBps: 10_001 }, "week over 100%"],
      [{ maxWeekBps: 1_000, maxDayBps: 2_000 }, "week under the day"],
      [{ maxWeightBps: 3_501 }, "weight over 35%"],
      [{ maxUnverifiedBps: 3_001 }, "unverified over 30%"],
      [{ maxWeightBps: 2_500, maxUnverifiedBps: 3_000 }, "unverified over the weight cap"],
      [{ minAssets: 1 }, "fewer than 2 assets"],
      [{ maxAssets: 15 }, "more than 14 assets, what a basket can hold"],
      [{ minAssets: 7, maxAssets: 6 }, "min over max"],
      [{ minAssets: 2, maxAssets: 2 }, "two assets at 35% can't fill the basket"],
      [{ stopBps: 400 }, "a stop under 5%"],
      [{ stopBps: 9_001 }, "a stop over 90%"],
    ];
    for (const [g, why] of bad) {
      await expectError(enableSmart(w.ctx, w.basket, w.creator, { ...GUARD, ...g }), "BadGuardrails").catch((e) => assert.fail(`${why}: ${e}`));
    }
    await expectError(enableSmart(w.ctx, w.basket, w.creator, GUARD, new Array(32).fill(0)), "BadGuardrails");
    await enableSmart(w.ctx, w.basket, w.creator);
    const b = await w.ctx.vault.account.basket.fetch(w.basket);
    assert.deepEqual(b.control, { smart: {} });
    const s = (await w.ctx.vault.account.smart.fetch(pda.smart(w.basket))) as any;
    assert.deepEqual(s.mandateHash, MANDATE);
    assert.equal(s.guard.maxDayBps, 2_000);
  });

  it("only on a basket already within its guardrails: weights and asset count (L7)", async () => {
    // 40% in A, over the 35% cap.
    const heavy = await fourAssets([4_000, 2_000, 2_000, 2_000]);
    await expectError(enableSmart(heavy.ctx, heavy.basket, heavy.creator, GUARD, MANDATE, null, [heavy.A, heavy.B]), "WeightAboveCap");
    // 35% in A is its Verified cap, but only with A's record named; without it, A is held to 30%.
    const w = await fourAssets([3_500, 2_500, 2_000, 2_000]);
    await expectError(enableSmart(w.ctx, w.basket, w.creator), "WeightAboveCap");
    // C has no record: 35% in C is over its 30% whatever is named.
    const c = await fourAssets([2_000, 2_500, 3_500, 2_000]);
    await expectError(enableSmart(c.ctx, c.basket, c.creator, GUARD, MANDATE, null, [c.A, c.B, c.C, c.D]), "WeightAboveCap");
    // Four assets held, three the most allowed.
    await expectError(enableSmart(w.ctx, w.basket, w.creator, { ...GUARD, minAssets: 3, maxAssets: 3 }, MANDATE, null, [w.A]), "AssetCountOutOfRange");
    await enableSmart(w.ctx, w.basket, w.creator, GUARD, MANDATE, null, [w.A]);
    assert.deepEqual((await w.ctx.vault.account.basket.fetch(w.basket)).control, { smart: {} });
  });

  it("can't happen once a basket has holders, nor to an immutable basket, nor at creation", async () => {
    const seeded = await world();
    await expectError(enableSmart(seeded.ctx, seeded.basket, seeded.creator), "SmartNotAllowed");
    const imm = await world({ seed: false, control: "immutable" });
    await expectError(enableSmart(imm.ctx, imm.basket, imm.creator), "SmartNotAllowed");
    await expectError(world({ seed: false, control: "smart" as any }), "SmartNotAllowed");
  });

  it("takes targets and assets out of the creator's hands, but leaves policy and stops", async () => {
    const w = await smartWorld();
    const reconf = { authority: w.creator.publicKey, basket: w.basket };
    await expectError(rpc(w.ctx, w.ctx.vault.methods.setTargets([4_000, 2_000, 2_000, 2_000]).accountsPartial(reconf), [w.creator]), "SmartManaged");
    await expectError(rpc(w.ctx, w.ctx.vault.methods.removeConstituent(3).accountsPartial(reconf), [w.creator]), "SmartManaged");
    await expectError(
      rpc(
        w.ctx,
        w.ctx.vault.methods
          .addConstituent()
          .accountsPartial({ ...NO_GRAVI, authority: w.creator.publicKey, basket: w.basket, mint: w.E, assetEntry: pda.asset(w.E), vault: ata(w.E, w.basket) }),
        [w.creator],
      ),
      "SmartManaged",
    );
    await rpc(w.ctx, w.ctx.vault.methods.setPolicy({ drift: {} }, new BN(0), 300, 0).accountsPartial(reconf), [w.creator]);
    await rpc(
      w.ctx,
      w.ctx.vault.methods.setStops([{ mint: w.A, pctBps: 2_000, trailing: false, referenceE12: new BN(E12.toString()) }]).accountsPartial({ creator: w.creator.publicKey, basket: w.basket, stopLoss: pda.stop(w.basket) }),
      [w.creator],
    );
  });
});

describe("smart baskets: the strategist", () => {
  it("is the one key the protocol authority names, and only on smart baskets", async () => {
    const w = await smartWorld();
    const stranger = fund(w.ctx);
    await expectError(setTargets(w, [3_000, 2_500, 2_500, 2_000], stranger), "NotStrategist");
    await expectError(setTargets(w, [3_000, 2_500, 2_500, 2_000], w.creator), "NotStrategist");
    await expectError(setSmartConfig(w.ctx, stranger.publicKey, 80, stranger), "NotProtocolAuthority");
    await setTargets(w, [3_000, 2_500, 2_500, 2_000]);
    assert.deepEqual(await targets(w), [3_000, 2_500, 2_500, 2_000]);
    // Removed, the old key can do nothing.
    await setSmartConfig(w.ctx, PublicKey.default, 80);
    await expectError(setTargets(w, [2_500, 2_500, 2_500, 2_500]), "NotStrategist");
  });

  it("can't act on a basket that isn't smart", async () => {
    const w = await world({ seed: false });
    const strategist = fund(w.ctx);
    await setSmartConfig(w.ctx, strategist.publicKey, 80);
    await expectError(
      rpc(
        w.ctx,
        w.ctx.vault.methods
          .smartSetTargets([5_000, 5_000])
          .accountsPartial({ strategist: strategist.publicKey, strategistConfig: pda.strategist(), basket: w.basket, smart: pda.smart(w.basket) })
          .remainingAccounts([w.mintA, w.mintB].map((m) => ({ pubkey: pda.verified(m), isSigner: false, isWritable: false }))),
        [strategist],
      ),
      "AccountNotInitialized",
    );
  });

  it("keeps each asset under its cap: 35% Verified, 30% not, and the right record", async () => {
    const w = await smartWorld();
    await expectError(setTargets(w, [3_600, 2_400, 2_000, 2_000]), "WeightAboveCap");
    await expectError(setTargets(w, [2_500, 1_500, 3_100, 2_900]), "WeightAboveCap");
    // C passed as Verified with A's record: refused, not taken for C's.
    await expectError(setTargets(w, [2_500, 1_500, 3_100, 2_900], w.strategist, [w.A, w.B, w.C, w.D].map((m, i) => pda.verified(i === 2 ? w.A : m))), "BadVerifiedRecord");
    await setTargets(w, [3_500, 2_000, 2_500, 2_000]);
    // Once C is Verified, it may go to 35%.
    await setVerified(w.ctx, w.C, true);
    await setTargets(w, [3_000, 1_500, 3_500, 2_000]);
    await setVerified(w.ctx, w.C, false);
    assert.equal(w.ctx.svm.getAccount(pda.verified(w.C))?.lamports ?? 0, 0, "the record closed");
    // Over its cap now (35%, the cap back to 30%): fine to hold or lower, not to raise.
    warp(w.ctx, DAY);
    await setTargets(w, [3_000, 1_600, 3_400, 2_000]);
    await expectError(setTargets(w, [3_000, 1_550, 3_450, 2_000]), "WeightAboveCap");
  });

  it("keeps the number of assets within the mandate", async () => {
    const w = await smartWorld({ ...GUARD, minAssets: 3, maxAssets: 4 });
    // Four held, the most allowed: adding a fifth at a weight is refused.
    await addAsset(w, w.E);
    await expectError(setTargets(w, [2_500, 2_500, 2_000, 2_000, 1_000]), "AssetCountOutOfRange");
    // Down to three, the least allowed, over two days (a quarter of the basket moves).
    await setTargets(w, [3_000, 3_000, 2_500, 1_500, 0]);
    warp(w.ctx, DAY);
    await setTargets(w, [3_500, 3_500, 3_000, 0, 0]);
    // Under a mandate asking for four, three may stay three while the strategist finds a fourth.
    await propose(w, hash("four"), { ...GUARD, minAssets: 4, maxAssets: 4 });
    warp(w.ctx, DAY);
    await apply(w);
    await setTargets(w, [3_500, 3_500, 3_000, 0, 0]);
    await setTargets(w, [3_500, 3_000, 3_000, 0, 500]);
    // From four, three is refused.
    const short = await smartWorld({ ...GUARD, minAssets: 4, maxAssets: 6 });
    await expectError(setTargets(short, [3_500, 3_500, 3_000, 0]), "AssetCountOutOfRange");
  });

  it("changes at most 20% in 24 hours", async () => {
    const w = await smartWorld();
    await setTargets(w, [3_500, 2_500, 2_000, 2_000]);
    await setTargets(w, [3_500, 3_500, 1_500, 1_500]);
    await expectError(setTargets(w, [3_500, 3_499, 1_501, 1_500]), "TurnoverDayCap");
    // In one go, too: 21% at once is refused on a fresh day.
    warp(w.ctx, DAY);
    await expectError(setTargets(w, [2_400, 2_500, 2_600, 2_500]), "TurnoverDayCap");
    await setTargets(w, [3_500, 3_499, 1_501, 1_500]);
    const s = await smartAccount(w);
    assert.equal(s.changes, 3);
  });

  it("changes at most 50% in 7 days", async () => {
    const w = await smartWorld();
    await setTargets(w, [3_500, 2_500, 2_000, 2_000]);
    await setTargets(w, [3_500, 3_500, 1_500, 1_500]); // 20% on day 1
    warp(w.ctx, DAY + HOUR);
    await setTargets(w, [2_500, 3_500, 2_000, 2_000]);
    await setTargets(w, [2_500, 2_500, 2_500, 2_500]); // 40% by day 2
    warp(w.ctx, DAY + HOUR);
    await expectError(setTargets(w, [3_100, 3_000, 2_000, 1_900]), "TurnoverWeekCap");
    await setTargets(w, [3_000, 3_000, 2_000, 2_000]); // 50% by day 3
    await expectError(setTargets(w, [3_001, 2_999, 2_000, 2_000]), "TurnoverWeekCap");
    // A week after day 1, its 20% has left the window.
    warp(w.ctx, 7 * DAY - 2 * (DAY + HOUR) + HOUR);
    await setTargets(w, [3_001, 2_999, 2_000, 2_000]);
  });

  it("adds assets without burning GRAVI, and removes only sold-out ones", async () => {
    const w = await smartWorld();
    await setCreationBurn(w.ctx, w.graviMint, 1_000_000_000n);
    const burned = async () => BigInt(((await w.ctx.vault.account.creationBurn.fetch(pda.creationBurn())) as any).totalBurned.toString());
    // No GRAVI accounts passed and none burned: a creator's add here would need them.
    await addAsset(w, w.E);
    assert.equal(await burned(), 0n);
    assert.equal((await basketMints(w)).length, 5);
    await expectError(addAsset(w, w.E), "DuplicateConstituent");
    const stranger = fund(w.ctx);
    await expectError(addAsset(w, w.A, stranger), "NotStrategist");
    // An unlisted token can't come in.
    const unlisted = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
    await createAta(w.ctx, unlisted, w.basket, TOKEN_PROGRAM_ID);
    await expectError(addAsset(w, unlisted), "AssetNotListed");
    await expectError(removeAsset(w, 0), "ConstituentNotEmpty");
    await setTargets(w, [2_500, 2_500, 2_500, 1_000, 1_500]);
    warp(w.ctx, DAY);
    await setTargets(w, [3_000, 2_500, 3_000, 0, 1_500]);
    await removeAsset(w, 3);
    assert.deepEqual((await basketMints(w)).map((m) => m.toBase58()), [w.A, w.B, w.C, w.E].map((m) => m.toBase58()));
    await expectError(removeAsset(w, 0, stranger), "NotStrategist");
  });

  it("puts the mandate's stop on an asset, below its fresh price, once", async () => {
    const w = await smartWorld();
    const keeper = fund(w.ctx);
    await initKeepers(w.ctx, keeper.publicKey, keeper.publicKey);
    await postPrices(w.ctx, keeper, [[w.A, 2n * E12]]);
    const setStop = (mint: PublicKey, signer: Keypair = w.strategist) =>
      rpc(
        w.ctx,
        w.ctx.vault.methods.smartSetStop().accountsPartial({
          strategist: signer.publicKey,
          strategistConfig: pda.strategist(),
          basket: w.basket,
          smart: pda.smart(w.basket),
          stopLoss: pda.stop(w.basket),
          assetPrice: pda.price(mint),
        }),
        [signer],
      );
    await expectError(setStop(w.A, fund(w.ctx)), "NotStrategist");
    await setStop(w.A);
    const st = (await w.ctx.vault.account.stopLoss.fetch(pda.stop(w.basket))) as any;
    assert.equal(st.stops.length, 1);
    assert.equal(st.stops[0].pctBps, 3_500);
    assert.equal(st.stops[0].trailing, true);
    assert.equal(st.stops[0].referenceE12.toString(), (2n * E12).toString());
    await expectError(setStop(w.A), "SmartStopNotAllowed");
    warp(w.ctx, 121);
    await rpc(w.ctx, w.ctx.vault.methods.setStops([]).accountsPartial({ creator: w.creator.publicKey, basket: w.basket, stopLoss: pda.stop(w.basket) }), [w.creator]);
    await expectError(setStop(w.A), "BadPrice");

    const noStop = await smartWorld({ ...GUARD, stopBps: 0 });
    const k2 = fund(noStop.ctx);
    await initKeepers(noStop.ctx, k2.publicKey, k2.publicKey);
    await postPrices(noStop.ctx, k2, [[noStop.A, E12]]);
    await expectError(
      rpc(
        noStop.ctx,
        noStop.ctx.vault.methods.smartSetStop().accountsPartial({
          strategist: noStop.strategist.publicKey,
          strategistConfig: pda.strategist(),
          basket: noStop.basket,
          smart: pda.smart(noStop.basket),
          stopLoss: pda.stop(noStop.basket),
          assetPrice: pda.price(noStop.A),
        }),
        [noStop.strategist],
      ),
      "SmartStopNotAllowed",
    );
  });
});

describe("smart baskets: pause and mandate", () => {
  it("pauses by the creator or the guardian, keeping the last targets; stops still fire", async () => {
    const w = await smartWorld();
    const stranger = fund(w.ctx);
    await expectError(pause(w, true, stranger), "NotPauser");
    await pause(w, true, w.creator);
    await expectError(setTargets(w, [3_000, 2_500, 2_500, 2_000]), "SmartPaused");
    await expectError(addAsset(w, w.E), "SmartPaused");
    await pause(w, false, w.creator);
    await setTargets(w, [3_000, 2_500, 2_500, 2_000]);
    // The guardian's pause: only the guardian lifts it.
    await pause(w, true, w.guardian);
    await expectError(pause(w, false, w.creator), "NotPauser");
    await expectError(setTargets(w, [2_500, 2_500, 2_500, 2_500]), "SmartPaused");

    // A stop fires on a paused basket.
    const keeper = fund(w.ctx);
    await initKeepers(w.ctx, keeper.publicKey, keeper.publicKey);
    await postPrices(w.ctx, keeper, [[w.A, E12]]);
    await rpc(
      w.ctx,
      w.ctx.vault.methods.setStops([{ mint: w.A, pctBps: 2_000, trailing: false, referenceE12: new BN(E12.toString()) }]).accountsPartial({ creator: w.creator.publicKey, basket: w.basket, stopLoss: pda.stop(w.basket) }),
      [w.creator],
    );
    const check = () =>
      rpc(
        w.ctx,
        w.ctx.vault.methods
          .checkStop()
          .accountsPartial({ basket: w.basket, stopLoss: pda.stop(w.basket), assetPrice: pda.price(w.A) })
          .remainingAccounts([pda.smart(w.basket), ...[w.A, w.B, w.C, w.D].map((m) => pda.verified(m))].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false }))),
      );
    await postPrices(w.ctx, keeper, [[w.A, (E12 * 75n) / 100n]]);
    await check();
    warp(w.ctx, 301);
    await postPrices(w.ctx, keeper, [[w.A, (E12 * 75n) / 100n]]);
    await check();
    // Fired while paused, as far as the caps let it: B to its 35%, C and D to their 30%; the 5%
    // nothing has room for stays on A.
    assert.deepEqual(await targets(w), [500, 3_500, 3_000, 3_000], "the stop fired while paused");

    await pause(w, false, w.guardian);
    await setTargets(w, [1_000, 3_500, 2_500, 3_000]);
  });

  it("a stop can't move a smart basket's weights past its guardrails (M1)", async () => {
    const w = await smartWorld();
    warp(w.ctx, DAY); // off LiteSVM's clock at 0, where an arming time reads as none
    const keeper = fund(w.ctx);
    await initKeepers(w.ctx, keeper.publicKey, keeper.publicKey);
    await postPrices(w.ctx, keeper, [[w.A, E12]]);
    // Before the first deposit a reference isn't checked against the price: the creator writes
    // one a hundred times A's price, so the stop arms at once.
    await rpc(
      w.ctx,
      w.ctx.vault.methods.setStops([{ mint: w.A, pctBps: 2_000, trailing: false, referenceE12: new BN((100n * E12).toString()) }]).accountsPartial({ creator: w.creator.publicKey, basket: w.basket, stopLoss: pda.stop(w.basket) }),
      [w.creator],
    );
    const accounts = { basket: w.basket, stopLoss: pda.stop(w.basket), assetPrice: pda.price(w.A) };
    const metas = (keys: PublicKey[]) => keys.map((pubkey) => ({ pubkey, isSigner: false, isWritable: false }));
    const records = [w.A, w.B, w.C, w.D].map((m) => pda.verified(m));
    await rpc(w.ctx, w.ctx.vault.methods.checkStop().accountsPartial(accounts));
    warp(w.ctx, 301);
    await postPrices(w.ctx, keeper, [[w.A, E12]]);
    // Firing needs the rules and every record, so it can't be made to move less than its rule.
    await expectError(rpc(w.ctx, w.ctx.vault.methods.checkStop().accountsPartial(accounts)), "BadRemainingAccounts");
    await expectError(rpc(w.ctx, w.ctx.vault.methods.checkStop().accountsPartial(accounts).remainingAccounts(metas(records))), "BadRemainingAccounts");
    await expectError(rpc(w.ctx, w.ctx.vault.methods.checkStop().accountsPartial(accounts).remainingAccounts(metas([pda.smart(w.basket), ...records.slice(1)]))), "BadRemainingAccounts");
    await rpc(w.ctx, w.ctx.vault.methods.checkStop().accountsPartial(accounts).remainingAccounts(metas([...records.reverse(), pda.smart(w.basket)])));
    // Scaled up in proportion, C and D (Unverified) would be 33% each, over their 30%.
    assert.deepEqual(await targets(w), [500, 3_500, 3_000, 3_000]);
  });

  it("changes the mandate only after a day's notice, and the guardrails with it", async () => {
    const w = await smartWorld();
    const next = hash('{"goal":"blue chips"}');
    const tighter = { ...GUARD, maxDayBps: 500 };
    await expectError(propose(w, next, tighter, false, w.strategist), "NotMandateAuthority");
    await expectError(propose(w, next, { ...GUARD, maxDayBps: 4_001 }), "BadGuardrails");
    await expectError(propose(w, next, { ...GUARD, maxDayBps: 4_000, maxWeekBps: 10_001 }), "BadGuardrails");
    await propose(w, next, { ...GUARD, maxDayBps: 4_000, maxWeekBps: 10_000 }); // the ceilings themselves
    await propose(w, next, tighter);
    await expectError(apply(w), "MandateNoticeRunning");
    // Until it applies, the old rules hold.
    await setTargets(w, [3_500, 2_500, 2_000, 2_000]);
    warp(w.ctx, DAY - 60);
    await expectError(apply(w), "MandateNoticeRunning");
    warp(w.ctx, 61);
    await apply(w);
    const s = await smartAccount(w);
    assert.deepEqual(s.mandateHash, next);
    assert.equal(s.guard.maxDayBps, 500);
    assert.equal(s.pendingAt.toString(), "0");
    await expectError(apply(w), "NoPendingMandate");
    await expectError(setTargets(w, [2_500, 2_500, 2_500, 2_500]), "TurnoverDayCap");
    await setTargets(w, [3_000, 2_500, 2_500, 2_000]);
  });

  it("the notice and the ceilings are params: a mandate waits the notice in force when proposed, and a raised ceiling admits wider guardrails after its week", async () => {
    const w = await smartWorld();
    const next = hash('{"goal":"wider"}');
    const wide = { ...GUARD, maxDayBps: 5_000, maxWeekBps: 10_000 };
    await expectError(propose(w, next, wide), "BadGuardrails");
    await setParam(w.ctx, PARAM.SMART_MAX_DAY_BPS, 5_000n);
    await setParam(w.ctx, PARAM.MANDATE_NOTICE_SECS, BigInt(2 * DAY));
    await expectError(propose(w, next, wide), "BadGuardrails");
    warp(w.ctx, PARAM_NOTICE);
    await propose(w, next, wide);
    const at = Number((await smartAccount(w)).pendingAt);
    assert.equal(at, now(w.ctx) + 2 * DAY);
    // Shortened again: the mandate already waiting keeps its two days.
    await setParam(w.ctx, PARAM.MANDATE_NOTICE_SECS, BigInt(6 * HOUR));
    warp(w.ctx, PARAM_NOTICE);
    assert.equal(Number((await smartAccount(w)).pendingAt), at);
  });

  it("the smart switch stops the strategist and new smart baskets; pausing and leaving still work", async () => {
    const w = await smartWorld();
    await setParam(w.ctx, PARAM.KILL, BigInt(FAMILY.smart));
    await expectError(setTargets(w, [3_500, 2_500, 2_000, 2_000]), "FeatureOff");
    await expectError(removeAsset(w, 0), "FeatureOff");
    await pause(w, true, w.creator);
    await pause(w, false, w.creator);
    const other = await fourAssets();
    await setParam(other.ctx, PARAM.KILL, BigInt(FAMILY.smart));
    await expectError(enableSmart(other.ctx, other.basket, other.creator), "FeatureOff");
    await propose(w, new Array(32).fill(0), GUARD, true);
    warp(w.ctx, DAY);
    await apply(w);
    assert.deepEqual((await w.ctx.vault.account.basket.fetch(w.basket)).control, { creator: {} });
  });

  it("withdraws a waiting mandate, and hands the basket back to its creator after notice", async () => {
    const w = await smartWorld();
    await propose(w, hash("x"), GUARD);
    await propose(w, new Array(32).fill(0), GUARD);
    await expectError(apply(w), "NoPendingMandate");
    await expectError(propose(w, new Array(32).fill(0), GUARD), "NoPendingMandate");

    await propose(w, new Array(32).fill(0), GUARD, true);
    warp(w.ctx, DAY);
    const before = w.ctx.svm.getBalance(w.creator.publicKey)!;
    await apply(w);
    assert.deepEqual((await w.ctx.vault.account.basket.fetch(w.basket)).control, { creator: {} });
    assert.equal(w.ctx.svm.getAccount(pda.smart(w.basket))?.lamports ?? 0, 0, "the rules account closed");
    assert.ok(w.ctx.svm.getBalance(w.creator.publicKey)! > before, "its rent back to the creator");
    await rpc(w.ctx, w.ctx.vault.methods.setTargets([4_000, 2_000, 2_000, 2_000]).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }), [w.creator]);
  });
});

describe("smart baskets: the fee", () => {
  async function seededSmart(protocolFeeBps = 40) {
    const w = await world({ seed: false, protocolFeeBps });
    forceSmart(w.ctx, w.basket);
    const seed = (shares: bigint) =>
      rpc(
        w.ctx,
        w.ctx.vault.methods
          .seedBasket(new BN(shares.toString()), [new BN(UNITS_A.toString()), new BN(UNITS_B.toString())])
          .accountsPartial({
            payer: w.creator.publicKey,
            basket: w.basket,
            shareMint: w.shareMint,
            payerShares: ata(w.shareMint, w.creator.publicKey),
            ...feeShares(w.shareMint),
            shareTokenProgram: TOKEN_PROGRAM_ID,
          })
          .remainingAccounts(quads(w, w.creator.publicKey)),
        [w.creator],
      );
    return { w, seed };
  }

  it("is 0.8% on every mint and redeem by default, at least half burned", async () => {
    const { w, seed } = await seededSmart(40);
    await seed(SEED_SHARES);
    const fee = (SEED_SHARES * 80n + 9_999n) / 10_000n;
    const { burnShares, treasuryShares } = feeShares(w.shareMint);
    assert.equal(balance(w.ctx, ata(w.shareMint, w.creator.publicKey)), SEED_SHARES - fee);
    assert.equal(balance(w.ctx, burnShares) + balance(w.ctx, treasuryShares), fee);
    assert.ok(balance(w.ctx, burnShares) * 2n >= fee);

    const redeem = 1_000_000_000n;
    const before = balance(w.ctx, burnShares) + balance(w.ctx, treasuryShares);
    await rpc(
      w.ctx,
      w.ctx.vault.methods
        .redeemInKind(new BN(redeem.toString()), 0)
        .accountsPartial({
          payer: w.creator.publicKey,
          basket: w.basket,
          shareMint: w.shareMint,
          payerShares: ata(w.shareMint, w.creator.publicKey),
          ...feeShares(w.shareMint),
          shareTokenProgram: TOKEN_PROGRAM_ID,
        })
        .remainingAccounts(quads(w, w.creator.publicKey)),
      [w.creator],
    );
    assert.equal(balance(w.ctx, burnShares) + balance(w.ctx, treasuryShares) - before, (redeem * 80n + 9_999n) / 10_000n);
  });

  it("a creator's cut comes on top: 1.40% on a buy at the cap, and a sale pays only the 0.8%", async () => {
    const { w, seed } = await seededSmart(40);
    await rpc(w.ctx, w.ctx.vault.methods.setCreatorCut(60).accountsPartial({ authority: w.creator.publicKey, basket: w.basket, notice: null }), [w.creator]);
    await seed(SEED_SHARES);
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await rpc(w.ctx, mintIx(w, alice, SEED_SHARES), [alice]);
    assert.equal(balance(w.ctx, ata(w.shareMint, alice.publicKey)), (SEED_SHARES * 9_860n) / 10_000n);
    const owed = (SEED_SHARES * 60n) / 10_000n;
    assert.equal((await w.ctx.vault.account.basket.fetch(w.basket)).creatorUnclaimed.toString(), owed.toString());
    const { burnShares, treasuryShares } = feeShares(w.shareMint);
    const before = balance(w.ctx, burnShares) + balance(w.ctx, treasuryShares);
    await rpc(w.ctx, redeemIx(w, alice, 1_000_000_000n), [alice]);
    assert.equal(balance(w.ctx, burnShares) + balance(w.ctx, treasuryShares) - before, 8_000_000n);
    assert.equal((await w.ctx.vault.account.basket.fetch(w.basket)).creatorUnclaimed.toString(), owed.toString());
  });

  it("is set by the protocol authority up to 1%, never past it", async () => {
    const { w, seed } = await seededSmart(40);
    await expectError(setSmartConfig(w.ctx, w.creator.publicKey, 101), "FeeTooHigh");
    await setSmartConfig(w.ctx, w.creator.publicKey, 100);
    await seed(SEED_SHARES);
    const fee = (SEED_SHARES * 100n + 9_999n) / 10_000n;
    assert.equal(balance(w.ctx, ata(w.shareMint, w.creator.publicKey)), SEED_SHARES - fee);
    // The normal fee keeps its own 0.5% ceiling.
    await expectError(
      rpc(w.ctx, protocolFeesIx(w.ctx, 51, 5_000)),
      "FeeTooHigh",
    );
    // A burn-exempt wallet set afterwards keeps the fee in place.
    await rpc(w.ctx, setRoleIx(w.ctx, ROLE.burnExempt, w.creator.publicKey, w.ctx.payer.publicKey)); skipNotices(w.ctx);
    const raw = Buffer.from(w.ctx.svm.getAccount(pda.protocol())!.data);
    assert.equal(raw.readUInt16LE(SMART_FEE_AT), 100);
  });

  it("leaves other baskets at the normal fee", async () => {
    const w = await world({ protocolFeeBps: 40 });
    await setSmartConfig(w.ctx, w.creator.publicKey, 100);
    const fee = (SEED_SHARES * 40n + 9_999n) / 10_000n;
    assert.equal(balance(w.ctx, ata(w.shareMint, w.creator.publicKey)), SEED_SHARES - fee);
  });
});

describe("smart baskets: the smart burn", () => {
  // The test GRAVI mint has 9 decimals.
  const GRAVI = 1_000_000_000n;
  const creationBurned = async (ctx: Ctx) => BigInt(((await (ctx.vault.account as any).creationBurn.fetch(pda.creationBurn())) as any).totalBurned.toString());

  it("starts off: a basket becomes smart without any GRAVI", async () => {
    const w = await fourAssets();
    await enableSmart(w.ctx, w.basket, w.creator, { ...GUARD, minAssets: 2 });
    assert.ok(await w.ctx.vault.account.smart.fetch(pda.smart(w.basket)));
  });

  it("is set by the protocol authority, up to 1,000,000 GRAVI, against GRAVI's own mint", async () => {
    const w = await world({ seed: false });
    const stranger = fund(w.ctx);
    await expectError(setSmartBurn(w.ctx, w.graviMint, GRAVI, stranger), "NotProtocolAuthority");
    await expectError(setSmartBurn(w.ctx, w.graviMint, 1_000_000n * GRAVI + 1n), "CreationBurnTooHigh");
    await expectError(setSmartBurn(w.ctx, await createMint(w.ctx, 9), GRAVI), "GraviMintNotSet");
    await setSmartBurn(w.ctx, w.graviMint, 1_000_000n * GRAVI);
  });

  it("burns once from the creator on becoming smart, counted with the creation burn; never without the GRAVI", async () => {
    const w = await fourAssets();
    const amount = 50_000n * GRAVI;
    await setSmartBurn(w.ctx, w.graviMint, amount);
    const guard = { ...GUARD, minAssets: 2 };
    // Without the GRAVI accounts, or short of GRAVI, it doesn't become smart.
    await expectError(enableSmart(w.ctx, w.basket, w.creator, guard), "CreationBurnAccountsMissing");
    const account = await createAta(w.ctx, w.graviMint, w.creator.publicKey);
    await mintTo(w.ctx, w.graviMint, account, amount - 1n);
    await expectError(enableSmart(w.ctx, w.basket, w.creator, guard, MANDATE, { mint: w.graviMint, account }), "insufficient funds");
    await mintTo(w.ctx, w.graviMint, account, 6n);
    // Another mint passed as GRAVI is refused.
    const other = await createMint(w.ctx, 9);
    const otherAccount = await createAta(w.ctx, other, w.creator.publicKey);
    await mintTo(w.ctx, other, otherAccount, amount);
    await expectError(enableSmart(w.ctx, w.basket, w.creator, guard, MANDATE, { mint: other, account: otherAccount }), "GraviMintNotSet");

    const supplyBefore = supply(w.ctx, w.graviMint);
    const countedBefore = await creationBurned(w.ctx);
    await enableSmart(w.ctx, w.basket, w.creator, guard, MANDATE, { mint: w.graviMint, account });
    assert.equal(balance(w.ctx, account), 5n);
    assert.equal(supply(w.ctx, w.graviMint), supplyBefore - amount);
    assert.equal(await creationBurned(w.ctx), countedBefore + amount);
  });

  it("burns nothing from the burn-exempt wallet (the team's)", async () => {
    const w = await fourAssets();
    await setSmartBurn(w.ctx, w.graviMint, 50_000n * GRAVI);
    await rpc(w.ctx, setRoleIx(w.ctx, ROLE.burnExempt, w.creator.publicKey, w.ctx.payer.publicKey)); skipNotices(w.ctx);
    await enableSmart(w.ctx, w.basket, w.creator, { ...GUARD, minAssets: 2 });
    assert.ok(await w.ctx.vault.account.smart.fetch(pda.smart(w.basket)));
  });

  it("sits after the Smart-mode fee: setting one keeps the other", async () => {
    const w = await world({ seed: false });
    const raw = () => Buffer.from(w.ctx.svm.getAccount(pda.protocol())!.data);
    // Set before any fee: the fee's bytes stay unset, so it reads as the default.
    await setSmartBurn(w.ctx, w.graviMint, 7n);
    let r = raw();
    assert.equal(r.readBigUInt64LE(SMART_FEE_AT + 3), 7n);
    assert.equal(r[SMART_FEE_AT + 2], 0);
    await setSmartConfig(w.ctx, fund(w.ctx).publicKey, 90);
    await setSmartBurn(w.ctx, w.graviMint, 9n);
    await setSmartConfig(w.ctx, fund(w.ctx).publicKey, 70);
    r = raw();
    assert.equal(r.readBigUInt64LE(SMART_FEE_AT + 3), 9n);
    assert.equal(r.readUInt16LE(SMART_FEE_AT), 70);
    assert.equal(r[SMART_FEE_AT + 2], 1);
  });
});

describe("smart baskets: Verified records", () => {
  it("are written and removed by the protocol authority only", async () => {
    const ctx = setup();
    await initProtocol(ctx);
    const mint = await createMint(ctx, 6);
    const stranger = fund(ctx);
    await expectError(setVerified(ctx, mint, true, stranger), "NotProtocolAuthority");
    await setVerified(ctx, mint, true);
    await setVerified(ctx, mint, true);
    // Not in the IDL (no instruction reads it as an account): bump, then the mint, after the discriminator.
    const rec = ctx.svm.getAccount(pda.verified(mint))!;
    assert.ok(new PublicKey(rec.owner).equals(ctx.vault.programId));
    assert.ok(new PublicKey(Buffer.from(rec.data).subarray(9, 41)).equals(mint));
    await expectError(setVerified(ctx, mint, false, stranger), "NotProtocolAuthority");
    await setVerified(ctx, mint, false);
    assert.equal(ctx.svm.getAccount(pda.verified(mint))?.lamports ?? 0, 0);
    // Another address passed as the record is refused.
    await expectError(
      rpc(ctx, ctx.vault.methods.setVerified(true).accountsPartial({ authority: ctx.payer.publicKey, protocol: pda.protocol(), mint, verified: pda.verified(stranger.publicKey) })),
      "BadVerifiedRecord",
    );
  });
});
