// Cross-chain trips (C1): value moving between a mixed basket's Solana side and its part. The trip
// key takes tokens out toward the targets, no further than the drift needs; mints wait, redeems take
// an IOU; what comes back settles at or above its floor; a trip past its deadline can be failed by
// anyone, and only the guardian or authority lets trips start again.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { SYSVAR_INSTRUCTIONS_PUBKEY, type Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import { createTransferCheckedInstruction } from "@solana/spl-token";
import { BN, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, ata, balance, createAta, createMint, expectError, fund, mintTo, pda, rpc, send, warp, postPrices } from "./helpers.ts";
import { NO_GRAVI, feeShares, fundHolder, initKeepers, listAsset, mintIx, quads, redeemIx, setRole, world, ROLE, SEED_SHARES } from "./fixtures.ts";
import { EID, OFT, wirePart } from "./_parts.ts";

// 10 shares backed by 20 A ($1) and 5 B ($6): $20 / $30, 40% / 60% against targets of 60 / 40.
// B is the part: a trip sells B for A, at most what moves both by 20 points: $10 of the $50.
const PRICE_A = 1_000_000_000_000n;
const PRICE_B = 6_000_000_000_000n;
const ONE_A = 1_000_000n;
const ONE_B = 1_000_000_000n;
const SHARE = 1_000_000_000n;

async function tripWorld(opts: { part?: boolean } = {}) {
  const w = await world();
  const keeper = fund(w.ctx);
  const tripKey = fund(w.ctx);
  // The bridge-out address: the one owner a trip's tokens go to, another key than the trip key's.
  const bridge = fund(w.ctx);
  const admin = fund(w.ctx);
  await initKeepers(w.ctx, keeper.publicKey, keeper.publicKey);
  await postPrices(w.ctx, keeper, [[w.mintA, PRICE_A], [w.mintB, PRICE_B]]);
  const alice = fund(w.ctx);
  await fundHolder(w, alice);
  // Funded before B's mint authority goes to the store.
  const bob = fund(w.ctx);
  await fundHolder(w, bob);
  await rpc(w.ctx, mintIx(w, alice, 2n * SHARE), [alice]);
  // The bridge-out address holds A to bring back, and a B account the part goes out to.
  const tripA = await createAta(w.ctx, w.mintA, bridge.publicKey, TOKEN_PROGRAM_ID);
  await mintTo(w.ctx, w.mintA, tripA, 1_000n * ONE_A, TOKEN_PROGRAM_ID);
  const tripB = await createAta(w.ctx, w.mintB, bridge.publicKey, TOKEN_2022_PROGRAM_ID);
  const keyB = await createAta(w.ctx, w.mintB, tripKey.publicKey, TOKEN_2022_PROGRAM_ID);
  await setRole(w.ctx, ROLE.partAdmin, admin.publicKey);
  await setRole(w.ctx, ROLE.partOft, OFT);
  await setRole(w.ctx, ROLE.trip, tripKey.publicKey);
  await setRole(w.ctx, ROLE.tripOut, bridge.publicKey);
  // B becomes a part: its mint authority handed to an OFT store our admin runs, with a peer.
  const { store, peer } = opts.part === false ? { store: PublicKey.unique(), peer: PublicKey.unique() } : await wirePart(w.ctx, w.mintB, admin.publicKey);
  return { ...w, keeper, tripKey, bridge, alice, bob, tripA, tripB, keyB, store, peer };
}
type TW = Awaited<ReturnType<typeof tripWorld>>;

const book = (w: TW) => [
  { pubkey: w.vaultA, isSigner: false, isWritable: false },
  { pubkey: pda.price(w.mintA), isSigner: false, isWritable: false },
  { pubkey: w.vaultB, isSigner: false, isWritable: false },
  { pubkey: pda.price(w.mintB), isSigner: false, isWritable: false },
];

/** A trip's id is the unix time it starts (trip.rs): `id` here names the test's nth trip. */
const tids = new WeakMap<object, Map<number, number>>();
const tid = (w: TW, id: number) => tids.get(w)?.get(id) ?? Number(w.ctx.svm.getClock().unixTimestamp);

function start(w: TW, nth: number, sellB: bigint, opts: { signer?: Keypair; refund?: bigint; to?: PublicKey; id?: number } = {}) {
  const signer = opts.signer ?? w.tripKey;
  const id = opts.id ?? Number(w.ctx.svm.getClock().unixTimestamp);
  if (!tids.has(w)) tids.set(w, new Map());
  tids.get(w)!.set(nth, id);
  return rpc(
    w.ctx,
    w.ctx.vault.methods
      .tripStart(id, 1, 0, new BN(sellB.toString()), 50, new BN((opts.refund ?? 0n).toString()), EID)
      .accountsPartial({
        keeper: signer.publicKey,
        parts: pda.parts(),
        basket: w.basket,
        shareMint: w.shareMint,
        trip: pda.trip(w.basket, id),
        sellMint: w.mintB,
        buyMint: w.mintA,
        sellVault: w.vaultB,
        keeperSell: opts.to ?? w.tripB,
        sellTokenProgram: TOKEN_2022_PROGRAM_ID,
        partStore: w.store,
        partPeer: w.peer,
      })
      .remainingAccounts(book(w)),
    [signer],
  );
}

/** The bridge-out address brings `a` of A back into the trip's own account. */
async function deliver(w: TW, id: number, a: bigint) {
  const vault = await createAta(w.ctx, w.mintA, pda.trip(w.basket, tid(w, id)), TOKEN_PROGRAM_ID);
  await send(w.ctx, [createTransferCheckedInstruction(w.tripA, w.mintA, vault, w.bridge.publicKey, a, 6, [], TOKEN_PROGRAM_ID)], [w.bridge]);
  return vault;
}

const settle = (w: TW, id: number, signer?: Keypair) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.tripSettle().accountsPartial({
      signer: (signer ?? w.ctx.payer).publicKey,
      basket: w.basket,
      shareMint: w.shareMint,
      trip: pda.trip(w.basket, tid(w, id)),
      keeper: w.tripKey.publicKey,
      buyMint: w.mintA,
      tripVault: ata(w.mintA, pda.trip(w.basket, tid(w, id)), TOKEN_PROGRAM_ID),
      buyVault: w.vaultA,
      sellVault: w.vaultB,
      buyTokenProgram: TOKEN_PROGRAM_ID,
      protocol: pda.protocol(),
      parts: pda.parts(),
    }),
    signer ? [signer] : [],
  );

const tripVaultOf = (w: TW, id: number) => ata(w.mintA, pda.trip(w.basket, tid(w, id)), TOKEN_PROGRAM_ID);
const gone = (w: TW, key: PublicKey) => (w.ctx.svm.getAccount(key)?.lamports ?? 0) === 0;
const failTrip = (w: TW, id: number) => rpc(w.ctx, w.ctx.vault.methods.tripFail().accountsPartial({ basket: w.basket, trip: pda.trip(w.basket, tid(w, id)), tripVault: tripVaultOf(w, id) }));
const clearTrip = (w: TW, id: number | null, signer?: Keypair) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.tripClear().accountsPartial({
      signer: (signer ?? w.ctx.payer).publicKey,
      protocol: pda.protocol(),
      basket: w.basket,
      trip: id === null ? null : pda.trip(w.basket, tid(w, id)),
      keeper: id === null ? null : w.tripKey.publicKey,
      tripVault: id === null ? null : tripVaultOf(w, id),
      tokenProgram: id === null ? null : TOKEN_PROGRAM_ID,
    }),
    signer ? [signer] : [],
  );

function redeemDuringTrip(w: TW, who: Keypair, shares: bigint, id: number) {
  const trip = pda.trip(w.basket, tid(w, id));
  return rpc(
    w.ctx,
    w.ctx.vault.methods
      .redeemInKind(new BN(shares.toString()), 0)
      .accountsPartial({ payer: who.publicKey, basket: w.basket, shareMint: w.shareMint, payerShares: ata(w.shareMint, who.publicKey, w.shareProgram), ...feeShares(w.shareMint, w.shareProgram), shareTokenProgram: w.shareProgram })
      .remainingAccounts([
        ...quads(w, who.publicKey),
        { pubkey: trip, isSigner: false, isWritable: true },
        { pubkey: pda.tripIou(trip, who.publicKey), isSigner: false, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
      ]),
    [who],
  );
}

const basketOf = (w: TW) => w.ctx.vault.account.basket.fetch(w.basket) as Promise<any>;
const tripOpen = async (w: TW) => ((await basketOf(w)).reserved[4] & 8) !== 0;

describe("trips: starting", () => {
  it("only the trip key, only with a part, only toward the targets, no further than the drift needs", async () => {
    const w = await tripWorld();
    const stranger = fund(w.ctx);
    await createAta(w.ctx, w.mintB, stranger.publicKey, TOKEN_2022_PROGRAM_ID);
    await expectError(start(w, 1, ONE_B, { signer: stranger }), "NotTripKeeper");
    // Only to the bridge-out address: not the trip key's own account, nor anyone else's.
    await expectError(start(w, 1, ONE_B, { to: w.keyB }), "NotBridgeOut");
    await expectError(start(w, 1, ONE_B, { to: ata(w.mintB, stranger.publicKey, TOKEN_2022_PROGRAM_ID) }), "NotBridgeOut");
    // $50 + the 2 shares alice bought ($10): 20 points of $60 is $12, so 2 B, and 2.1 B is too much.
    await expectError(start(w, 1, 21n * ONE_B / 10n), "SliceTooLarge");
    // The bridge's fees: at most $2 of refund.
    await expectError(start(w, 1, ONE_B, { refund: ONE_B / 2n }), "RefundTooLarge");
    await start(w, 1, 15n * ONE_B / 10n, { refund: ONE_B / 10n });
    assert.equal(balance(w.ctx, w.tripB), 16n * ONE_B / 10n, "out to the bridge-out address, the refund with it");
    assert.ok(await tripOpen(w));
    const t = (await w.ctx.vault.account.trip.fetch(pda.trip(w.basket, tid(w, 1)))) as any;
    assert.equal(BigInt(t.minBuy.toString()), 8_955_000n, "$9 of A, less 0.5%");
    // One trip at a time (a second later: a trip's id is the second it starts).
    warp(w.ctx, 1);
    await expectError(start(w, 2, ONE_B / 10n), "TripOpen");
    // An id that isn't the start's own time is refused.
    await expectError(start(w, 3, ONE_B / 10n, { id: 1 }), "BadTripId");
  });

  it("refuses a pair without a part", async () => {
    const w = await tripWorld({ part: false });
    await expectError(start(w, 1, ONE_B), "TripNeedsPart");
  });
});

describe("trips: while out, and settling", () => {
  it("mints wait, redeems take an IOU, what comes back settles at or above its floor and the IOUs claim it", async () => {
    const w = await tripWorld();
    const bob = w.bob;
    await start(w, 1, 15n * ONE_B / 10n);
    await expectError(rpc(w.ctx, mintIx(w, bob, SHARE), [bob]), "TripOpen");
    // A redeem without the trip's accounts is refused; with them, alice's 1 share gets an IOU.
    await expectError(rpc(w.ctx, redeemIx(w, w.alice, SHARE), [w.alice]), "BadRemainingAccounts");
    await redeemDuringTrip(w, w.alice, SHARE, 1);
    const iou = (await w.ctx.vault.account.tripIou.fetch(pda.tripIou(pda.trip(w.basket, tid(w, 1)), w.alice.publicKey))) as any;
    assert.equal(BigInt(iou.shares.toString()), SHARE);
    // Short of the floor: refused. At it: settles.
    await deliver(w, 1, 8_000_000n);
    await expectError(settle(w, 1), "BelowOracleFloor");
    const before = balance(w.ctx, w.vaultA);
    await deliver(w, 1, 1_000_000n);
    await settle(w, 1);
    assert.ok(!(await tripOpen(w)));
    // 12 shares at the start, 1 redeemed while out: 1/12 of the 9 A stays for alice.
    const pool = 9_000_000n / 12n;
    assert.equal(balance(w.ctx, w.vaultA) - before, 9_000_000n - pool);
    await rpc(w.ctx, mintIx(w, bob, SHARE), [bob]);
    const aliceA = ata(w.mintA, w.alice.publicKey, TOKEN_PROGRAM_ID);
    const had = balance(w.ctx, aliceA);
    await rpc(
      w.ctx,
      w.ctx.vault.methods.tripClaim().accountsPartial({
        wallet: w.alice.publicKey,
        trip: pda.trip(w.basket, tid(w, 1)),
        iou: pda.tripIou(pda.trip(w.basket, tid(w, 1)), w.alice.publicKey),
        keeper: w.tripKey.publicKey,
        buyMint: w.mintA,
        tripVault: ata(w.mintA, pda.trip(w.basket, tid(w, 1)), TOKEN_PROGRAM_ID),
        walletBuy: aliceA,
        buyTokenProgram: TOKEN_PROGRAM_ID,
      }),
      [w.alice],
    );
    assert.equal(balance(w.ctx, aliceA) - had, pool);
    assert.equal(w.ctx.svm.getAccount(pda.trip(w.basket, tid(w, 1)))?.lamports ?? 0, 0, "the last IOU closes the trip");
    assert.ok(gone(w, tripVaultOf(w, 1)), "and the trip's token account, its rent to the keeper");
  });

  it("a trip with no IOUs closes its token account as it settles, the rent to the keeper", async () => {
    const w = await tripWorld();
    await start(w, 1, ONE_B);
    const vault = await deliver(w, 1, 6_000_000n);
    const rent = w.ctx.svm.getAccount(vault)!.lamports;
    const had = w.ctx.svm.getAccount(w.tripKey.publicKey)!.lamports;
    await settle(w, 1);
    assert.ok(gone(w, pda.trip(w.basket, tid(w, 1))));
    assert.ok(gone(w, vault));
    assert.ok(w.ctx.svm.getAccount(w.tripKey.publicKey)!.lamports - had >= BigInt(rent), "the keeper got the account's rent back too");
  });

  it("the burn's and the stakers' redeems wait for a trip (they take no IOU)", async () => {
    const w = await tripWorld();
    await start(w, 1, ONE_B);
    const burn = pda.burn();
    const burnA = await createAta(w.ctx, w.mintA, burn, TOKEN_PROGRAM_ID);
    const burnB = await createAta(w.ctx, w.mintB, burn, TOKEN_2022_PROGRAM_ID);
    const ix = w.ctx.vault.methods
      .buybackRedeem(new BN(1))
      .accountsPartial({ basket: w.basket, shareMint: w.shareMint, burnShares: feeShares(w.shareMint, w.shareProgram).burnShares, shareTokenProgram: w.shareProgram })
      .remainingAccounts(
        [
          [w.vaultA, burnA, w.mintA, TOKEN_PROGRAM_ID],
          [w.vaultB, burnB, w.mintB, TOKEN_2022_PROGRAM_ID],
        ].flatMap((row) => row.map((pubkey, i) => ({ pubkey, isSigner: false, isWritable: i < 2 }))),
      );
    await expectError(rpc(w.ctx, ix), "TripOpen");
  });

  it("slices wait for a trip", async () => {
    const w = await tripWorld();
    await start(w, 1, ONE_B);
    const ix = await w.ctx.vault.methods
      .rebalanceStart(1, 0, new BN(ONE_B.toString()), 50, new BN(0))
      .accountsPartial({ rebalancer: w.keeper.publicKey, keepers: pda.keepers(), basket: w.basket, rebalanceState: pda.rebalance(w.basket), sellMint: w.mintB, buyMint: w.mintA, sellVault: w.vaultB, rebalancerSell: ata(w.mintB, w.keeper.publicKey, TOKEN_2022_PROGRAM_ID), sellTokenProgram: TOKEN_2022_PROGRAM_ID, instructions: SYSVAR_INSTRUCTIONS_PUBKEY })
      .remainingAccounts(book(w))
      .instruction();
    await expectError(send(w.ctx, [ix], [w.keeper]), "TripOpen");
  });

  it("no asset comes or goes, nor any weight changes, while a trip is out (bug bounty, 10 Nov)", async () => {
    const w = await tripWorld();
    await start(w, 1, ONE_B);
    const reconf = { authority: w.creator.publicKey, basket: w.basket };
    await expectError(rpc(w.ctx, w.ctx.vault.methods.setTargets([5_000, 5_000]).accountsPartial(reconf), [w.creator]), "TripOpen");
    await expectError(rpc(w.ctx, w.ctx.vault.methods.removeConstituent(1).accountsPartial(reconf), [w.creator]), "TripOpen");
    const C = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
    await listAsset(w.ctx, C);
    const vaultC = await createAta(w.ctx, C, w.basket, TOKEN_PROGRAM_ID);
    await expectError(
      rpc(w.ctx, w.ctx.vault.methods.addConstituent().accountsPartial({ ...reconf, mint: C, assetEntry: pda.asset(C), vault: vaultC, ...NO_GRAVI }), [w.creator]),
      "TripOpen",
    );
  });
});

describe("trips: a trip that doesn't come back", () => {
  it("anyone fails it past its deadline; it stays out (mints wait) until it settles late; no new trip until cleared", async () => {
    const w = await tripWorld();
    await start(w, 1, ONE_B);
    const fail = () => failTrip(w, 1);
    await expectError(fail(), "TripNotDue");
    warp(w.ctx, 2 * 3_600 + 1);
    await fail();
    // Still out: the vaults miss what's away, so a mint now would buy at a discount.
    assert.ok(await tripOpen(w));
    await postPrices(w.ctx, w.keeper, [[w.mintA, PRICE_A], [w.mintB, PRICE_B]]);
    await expectError(rpc(w.ctx, mintIx(w, w.alice, SHARE), [w.alice]), "TripOpen");
    await expectError(start(w, 2, ONE_B / 10n), "TripOpen");
    // Late, and short of the floor: settles anyway, in public, but only by the trip's own keys,
    // the guardian or the authority: a stranger's unit of dust can't settle it.
    const stranger = fund(w.ctx);
    await deliver(w, 1, 1_000_000n);
    await expectError(settle(w, 1, stranger), "LateSettleNotAllowed");
    await settle(w, 1, w.bridge);
    assert.ok(gone(w, tripVaultOf(w, 1)), "no IOUs: its token account closed with it");
    // Settled late: mints reopen; still stuck until cleared.
    assert.ok(!(await tripOpen(w)));
    await rpc(w.ctx, mintIx(w, w.alice, SHARE), [w.alice]);
    await expectError(start(w, 2, ONE_B / 10n), "TripStuck");
    // Only the guardian or the authority clears it.
    // The settled trip closed itself (no IOUs); the stuck flag still needs clearing.
    assert.equal(w.ctx.svm.getAccount(pda.trip(w.basket, tid(w, 1)))?.lamports ?? 0, 0);
    await expectError(clearTrip(w, null, w.alice), "NotPauser");
    await clearTrip(w, null);
    await start(w, 2, ONE_B / 10n);
    void SEED_SHARES;
  });

  it("a failed trip nothing came back from is closed when cleared, and its IOUs give their rent back", async () => {
    const w = await tripWorld();
    await start(w, 1, ONE_B);
    await redeemDuringTrip(w, w.alice, SHARE, 1);
    const iou = pda.tripIou(pda.trip(w.basket, tid(w, 1)), w.alice.publicKey);
    const closeIou = () => rpc(w.ctx, w.ctx.vault.methods.tripIouClose().accountsPartial({ wallet: w.alice.publicKey, iou, trip: pda.trip(w.basket, tid(w, 1)) }), [w.alice]);
    await expectError(closeIou(), "TripStillOpen");
    warp(w.ctx, 2 * 3_600 + 1);
    await failTrip(w, 1);
    await clearTrip(w, 1);
    assert.ok(gone(w, pda.trip(w.basket, tid(w, 1))));
    assert.equal(((await basketOf(w)).reserved[4] & 16), 0);
    const rent = w.ctx.svm.getAccount(iou)!.lamports;
    const had = w.ctx.svm.getAccount(w.alice.publicKey)!.lamports;
    await closeIou();
    assert.ok(gone(w, iou));
    assert.ok(w.ctx.svm.getAccount(w.alice.publicKey)!.lamports - had >= BigInt(rent) - 10_000n, "the IOU's rent back to the redeemer");
  });

  it("a failed trip something came back to can't be cleared away: it settles, and its IOUs claim", async () => {
    const w = await tripWorld();
    await start(w, 1, ONE_B);
    warp(w.ctx, 2 * 3_600 + 1);
    await failTrip(w, 1);
    // An empty account (made, nothing in it) closes with the trip; one with tokens refuses.
    await deliver(w, 1, 2_000_000n);
    await expectError(clearTrip(w, 1), "TripVaultNotEmpty");
    await settle(w, 1, w.tripKey);
    await clearTrip(w, null);
    assert.ok(gone(w, pda.trip(w.basket, tid(w, 1))));
  });

  it("an empty trip account is closed with the trip when it's cleared", async () => {
    const w = await tripWorld();
    await start(w, 1, ONE_B);
    warp(w.ctx, 2 * 3_600 + 1);
    await failTrip(w, 1);
    const vault = await createAta(w.ctx, w.mintA, pda.trip(w.basket, tid(w, 1)), TOKEN_PROGRAM_ID);
    await clearTrip(w, 1);
    assert.ok(gone(w, vault));
    assert.ok(gone(w, pda.trip(w.basket, tid(w, 1))));
  });
});

describe("trips: the check-up's hardening (9-10 Nov)", () => {
  const sweep = (w: TW, id: number) =>
    rpc(
      w.ctx,
      w.ctx.vault.methods.tripSweep(tid(w, id)).accountsPartial({
        signer: w.ctx.payer.publicKey,
        basket: w.basket,
        trip: pda.trip(w.basket, tid(w, id)),
        tripVault: tripVaultOf(w, id),
        basketVault: w.vaultA,
        mint: w.mintA,
        tokenProgram: TOKEN_PROGRAM_ID,
        keeper: w.tripKey.publicKey,
      }),
    );

  it("a trip whose floor already arrived can't be failed: it settles", async () => {
    const w = await tripWorld();
    await start(w, 1, ONE_B);
    await deliver(w, 1, 9_000_000n);
    warp(w.ctx, 2 * 3_600 + 1);
    await expectError(failTrip(w, 1), "TripDelivered");
    await settle(w, 1);
    assert.ok(!(await tripOpen(w)));
  });

  it("a failed trip still out must be named to be cleared; redeems after the fail still take IOUs", async () => {
    const w = await tripWorld();
    await start(w, 1, ONE_B);
    warp(w.ctx, 2 * 3_600 + 1);
    await failTrip(w, 1);
    await redeemDuringTrip(w, w.alice, SHARE, 1);
    const iou = (await w.ctx.vault.account.tripIou.fetch(pda.tripIou(pda.trip(w.basket, tid(w, 1)), w.alice.publicKey))) as any;
    assert.equal(BigInt(iou.shares.toString()), SHARE);
    await expectError(clearTrip(w, null), "TripStillOpen");
    await clearTrip(w, 1);
    assert.ok(!(await tripOpen(w)));
  });

  it("what reaches a trip's account after it was cleared goes to the basket (trip_sweep)", async () => {
    const w = await tripWorld();
    await start(w, 1, ONE_B);
    warp(w.ctx, 2 * 3_600 + 1);
    await failTrip(w, 1);
    await clearTrip(w, 1);
    // The bridge delivers late, to an account whose trip is gone.
    await deliver(w, 1, 3_000_000n);
    const before = balance(w.ctx, w.vaultA);
    await sweep(w, 1);
    assert.equal(balance(w.ctx, w.vaultA) - before, 3_000_000n);
    assert.ok(gone(w, tripVaultOf(w, 1)));
  });

  it("a trip still out can't be swept", async () => {
    const w = await tripWorld();
    await start(w, 1, ONE_B);
    await deliver(w, 1, 1_000_000n);
    await expectError(sweep(w, 1), "TripStillOpen");
  });
});

