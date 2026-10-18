// Automatic rebalancing: the vault lends an overweight sleeve to the rebalancer, who swaps
// it on the market in the same transaction, and the vault checks it got back at least the
// oracle value (minus a small tolerance) of the underweight sleeve.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { SYSVAR_INSTRUCTIONS_PUBKEY, type Keypair, PublicKey } from "@solana/web3.js";
import { createTransferCheckedInstruction } from "@solana/spl-token";
import { Clock } from "litesvm";
import {
  BN,
  ONE_SHARE,
  TOKEN_2022_PROGRAM_ID,
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
  rpc,
  send,
  setup,
  warp,
} from "./helpers.ts";
import { createFeeAccounts, feeShares, initProtocol, listAsset, world, type World } from "./fixtures.ts";

// World: 10 shares backed by 20 A ($1, 6 decimals) and 5 B ($6, 9 decimals): $20 / $30,
// so 40% A / 60% B against targets of 60 / 40. B must be sold for A.
const PRICE_A = 1_000_000_000_000n; // $1.00, x1e12
const PRICE_B = 6_000_000_000_000n; // $6.00
const ONE_A = 1_000_000n;
const ONE_B = 1_000_000_000n;

async function keeperWorld() {
  const w = await world();
  const keeper = fund(w.ctx);
  await rpc(
    w.ctx,
    w.ctx.vault.methods
      .initKeepers(keeper.publicKey, keeper.publicKey)
      .accountsPartial({ authority: w.ctx.payer.publicKey, keepers: pda.keepers() }),
  );
  await setPrices(w, keeper, PRICE_A, PRICE_B);
  // The keeper holds A to deliver, and has a B account to receive the lent sleeve.
  const keeperA = await createAta(w.ctx, w.mintA, keeper.publicKey, TOKEN_PROGRAM_ID);
  await mintTo(w.ctx, w.mintA, keeperA, 1_000n * ONE_A, TOKEN_PROGRAM_ID);
  const keeperB = await createAta(w.ctx, w.mintB, keeper.publicKey, TOKEN_2022_PROGRAM_ID);
  return { ...w, keeper, keeperA, keeperB };
}
type KW = Awaited<ReturnType<typeof keeperWorld>>;

async function setPrices(w: World, keeper: Keypair, a: bigint, b: bigint) {
  for (const [mint, price] of [[w.mintA, a], [w.mintB, b]] as const) {
    await rpc(
      w.ctx,
      w.ctx.vault.methods.setPrice(new BN(price.toString())).accountsPartial({
        priceAuthority: keeper.publicKey,
        keepers: pda.keepers(),
        mint,
        assetPrice: pda.price(mint),
      }),
      [keeper],
    );
  }
}

const book = (w: World) => [
  { pubkey: w.vaultA, isSigner: false, isWritable: false },
  { pubkey: pda.price(w.mintA), isSigner: false, isWritable: false },
  { pubkey: w.vaultB, isSigner: false, isWritable: false },
  { pubkey: pda.price(w.mintB), isSigner: false, isWritable: false },
];

function startIx(w: KW, sellAmount: bigint, opts: { signer?: PublicKey; sell?: number; buy?: number; slippage?: number } = {}) {
  const sell = opts.sell ?? 1;
  const [sellMint, sellVault, sellProgram, receive] =
    sell === 1 ? [w.mintB, w.vaultB, TOKEN_2022_PROGRAM_ID, w.keeperB] : [w.mintA, w.vaultA, TOKEN_PROGRAM_ID, w.keeperA];
  const buy = opts.buy ?? 1 - sell;
  return w.ctx.vault.methods
    .rebalanceStart(sell, buy, new BN(sellAmount.toString()), opts.slippage ?? 50)
    .accountsPartial({
      rebalancer: opts.signer ?? w.keeper.publicKey,
      keepers: pda.keepers(),
      basket: w.basket,
      rebalanceState: pda.rebalance(w.basket),
      sellMint,
      buyMint: buy === 1 ? w.mintB : w.mintA,
      sellVault,
      rebalancerSell: receive,
      sellTokenProgram: sellProgram,
      instructions: SYSVAR_INSTRUCTIONS_PUBKEY,
    })
    .remainingAccounts(book(w))
    .instruction();
}

function finishIx(w: KW, signer?: PublicKey) {
  return w.ctx.vault.methods
    .rebalanceFinish()
    .accountsPartial({
      rebalancer: signer ?? w.keeper.publicKey,
      keepers: pda.keepers(),
      basket: w.basket,
      shareMint: w.shareMint,
      rebalanceState: pda.rebalance(w.basket),
    })
    .remainingAccounts(book(w))
    .instruction();
}

/** The "market": the keeper delivers `amountA` of A into the vault. */
const deliverA = (w: KW, amountA: bigint) =>
  createTransferCheckedInstruction(w.keeperA, w.mintA, w.vaultA, w.keeper.publicKey, amountA, 6, [], TOKEN_PROGRAM_ID);

/** Sells $2.50 of B (the 5% slice cap on a $50 basket) for A at the oracle price. */
const SLICE_B = 416_666_666n; // 0.4166 B = $2.4999
const FAIR_A = 2_500_000n; // 2.5 A

describe("rebalancing", () => {
  it("a slice sells the overweight sleeve at the oracle price and re-derives units", async () => {
    const w = await keeperWorld();
    const [vA, vB] = [balance(w.ctx, w.vaultA), balance(w.ctx, w.vaultB)];
    await send(w.ctx, [await startIx(w, SLICE_B), deliverA(w, FAIR_A), await finishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.vaultB), vB - SLICE_B);
    assert.equal(balance(w.ctx, w.vaultA), vA + FAIR_A);
    const b = await w.ctx.vault.account.basket.fetch(w.basket);
    // 10 shares: units are balance / 10.
    assert.equal(b.constituents[0].unitsPerShare.toString(), ((vA + FAIR_A) / 10n).toString());
    assert.equal(b.constituents[1].unitsPerShare.toString(), ((vB - SLICE_B) / 10n).toString());
    const st = await w.ctx.vault.account.rebalanceState.fetch(pda.rebalance(w.basket));
    assert.equal(st.pending, false);
    assert.equal(st.sessionActive, true);
  });

  it("a fill below the oracle floor reverts the whole slice", async () => {
    const w = await keeperWorld();
    const vB = balance(w.ctx, w.vaultB);
    // 2% short of fair value, beyond the 0.5% tolerance: a sandwiched swap looks like this.
    await expectError(send(w.ctx, [await startIx(w, SLICE_B), deliverA(w, 2_450_000n), await finishIx(w)], [w.keeper]), "BelowOracleFloor");
    assert.equal(balance(w.ctx, w.vaultB), vB, "nothing left the vault");
  });

  it("a start without a finish in the same transaction is refused", async () => {
    const w = await keeperWorld();
    await expectError(send(w.ctx, [await startIx(w, SLICE_B)], [w.keeper]), "FinishMissing");
  });

  it("only the configured rebalancer can rebalance", async () => {
    const w = await keeperWorld();
    const outsider = fund(w.ctx);
    await expectError(
      send(w.ctx, [await startIx(w, SLICE_B, { signer: outsider.publicKey }), await finishIx(w, outsider.publicKey)], [outsider]),
      "NotRebalancer",
    );
  });

  it("a slice above 5% of the basket is refused", async () => {
    const w = await keeperWorld();
    await expectError(send(w.ctx, [await startIx(w, 2n * SLICE_B), deliverA(w, 2n * FAIR_A), await finishIx(w)], [w.keeper]), "SliceTooLarge");
  });

  it("the underweight sleeve can't be sold", async () => {
    const w = await keeperWorld();
    await expectError(send(w.ctx, [await startIx(w, 2n * ONE_A, { sell: 0 }), await finishIx(w)], [w.keeper]), "WrongDirection");
  });

  it("stale prices are refused", async () => {
    const w = await keeperWorld();
    warp(w.ctx, 200);
    await expectError(send(w.ctx, [await startIx(w, SLICE_B), deliverA(w, FAIR_A), await finishIx(w)], [w.keeper]), "BadPrice");
  });

  it("a basket within its drift tolerance and not yet due is left alone", async () => {
    const w = await keeperWorld();
    // B at $2.90: $20 A / $14.50 B = 58% / 42%, 2 points of drift against a 5-point tolerance.
    await setPrices(w, w.keeper, PRICE_A, 2_900_000_000_000n);
    await expectError(send(w.ctx, [await startIx(w, 10_000_000n), deliverA(w, 29_000n), await finishIx(w)], [w.keeper]), "RebalanceNotDue");
  });

  it("slices continue until every sleeve is on target, then the session ends", async () => {
    const w = await keeperWorld();
    const t0 = now(w.ctx);
    let slices = 0;
    for (; slices < 10; slices++) {
      const st = slices ? await w.ctx.vault.account.rebalanceState.fetch(pda.rebalance(w.basket)) : null;
      if (st && !st.sessionActive) break;
      // Each slice: the smaller of 5% of value and the remaining gap, sold at the oracle.
      const [a, b] = [balance(w.ctx, w.vaultA), balance(w.ctx, w.vaultB)];
      const valueA = (a * PRICE_A) / ONE_A;
      const valueB = (b * PRICE_B) / ONE_B;
      const total = valueA + valueB;
      const gap = (valueB * 10_000n) / total - 4_000n; // B's excess in bps
      const bps = gap < 500n ? gap : 499n;
      const sellValue = (total * bps) / 10_000n;
      const sellB = (sellValue * ONE_B) / PRICE_B;
      const getA = (sellValue * ONE_A) / PRICE_A;
      await send(w.ctx, [await startIx(w, sellB), deliverA(w, getA), await finishIx(w)], [w.keeper]);
    }
    const st = await w.ctx.vault.account.rebalanceState.fetch(pda.rebalance(w.basket));
    assert.equal(st.sessionActive, false, `session still open after ${slices} slices`);
    assert.ok(slices >= 4 && slices <= 6, `took ${slices} slices`);
    const b = await w.ctx.vault.account.basket.fetch(w.basket);
    assert.ok(b.lastRebalance.toNumber() >= t0, "last_rebalance moved");
    // Balanced and not due: the next attempt is refused.
    await expectError(send(w.ctx, [await startIx(w, 1_000_000n), deliverA(w, 6_000n), await finishIx(w)], [w.keeper]), "RebalanceNotDue");
  });

  it("holders can still mint and redeem after a rebalance", async () => {
    const w = await keeperWorld();
    await send(w.ctx, [await startIx(w, SLICE_B), deliverA(w, FAIR_A), await finishIx(w)], [w.keeper]);
    const { fundHolder, mintIx, redeemIx } = await import("./fixtures.ts");
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await rpc(w.ctx, mintIx(w, alice, 1_000_000_000n), [alice]);
    await rpc(w.ctx, redeemIx(w, alice, 1_000_000_000n), [alice]);
    assert.equal(balance(w.ctx, ata(w.shareMint, alice.publicKey)), 0n);
  });
});

/** One slice moving `bps` of the basket's value from B to A, at the oracle price. */
async function sliceBps(w: KW, bps: bigint) {
  const [a, b] = [balance(w.ctx, w.vaultA), balance(w.ctx, w.vaultB)];
  const total = (a * PRICE_A) / ONE_A + (b * PRICE_B) / ONE_B;
  const value = (total * bps) / 10_000n;
  await send(w.ctx, [await startIx(w, (value * ONE_B) / PRICE_B), deliverA(w, (value * ONE_A) / PRICE_A), await finishIx(w)], [w.keeper]);
}

/** Moves the chain on by `slots` without moving the clock, so prices stay fresh. */
function skipSlots(w: World, slots: bigint) {
  const c = w.ctx.svm.getClock();
  w.ctx.svm.setClock(new Clock(c.slot + slots, c.epochStartTimestamp, c.epoch, c.leaderScheduleEpoch, c.unixTimestamp));
  w.ctx.svm.expireBlockhash();
}

describe("rebalancing limits", () => {
  it("a slice too small to have a floor is refused", async () => {
    const w = await keeperWorld();
    // One base unit of B is worth $0.000000006: its floor in A rounds to nothing.
    await expectError(send(w.ctx, [await startIx(w, 1n), await finishIx(w)], [w.keeper]), "SliceTooSmall");
  });

  it("a session that hasn't traded for a day lapses", async () => {
    const w = await keeperWorld();
    // 20 points of drift down to 4.83, under the basket's 5-point tolerance and not yet due
    // by its schedule: only the open session lets it keep trading.
    for (const bps of [499n, 499n, 499n, 20n]) await sliceBps(w, bps);
    await sliceBps(w, 10n);
    skipSlots(w, 216_001n);
    await expectError(sliceBps(w, 10n), "RebalanceNotDue");
  });
});

// Four $1 tokens at 25% each. Seeded at 26.00 / 24.18 / 24.91 / 24.91, one slice of 0.73 from
// the first to the second leaves +0.27 / -0.09 / -0.09 / -0.09: one sleeve over target with no
// sleeve far enough under to buy.
async function fourWorld() {
  const ctx = setup();
  await initProtocol(ctx, 0, 5_000);
  const mints: PublicKey[] = [];
  for (let i = 0; i < 4; i++) {
    const m = await createMint(ctx, 6, TOKEN_PROGRAM_ID);
    await listAsset(ctx, m);
    mints.push(m);
  }
  const creator = fund(ctx);
  const id = new Uint8Array(16).fill(9);
  const basket = pda.basket(creator.publicKey, id);
  const shareMint = pda.share(basket);
  await rpc(
    ctx,
    ctx.vault.methods
      .initializeBasket({
        basketId: Array.from(id),
        access: { public: {} },
        control: { creator: {} },
        rebalanceMode: { drift: {} },
        intervalSecs: new BN(0),
        driftBps: 50,
        mgmtFeeBps: 0,
        mints,
        targetBps: [2_500, 2_500, 2_500, 2_500],
      })
      .accountsPartial({ creator: creator.publicKey, basket, shareMint, shareTokenProgram: TOKEN_PROGRAM_ID })
      .remainingAccounts(mints.flatMap((m) => [
        { pubkey: m, isSigner: false, isWritable: false },
        { pubkey: pda.asset(m), isSigner: false, isWritable: false },
      ])),
    [creator],
  );
  await createFeeAccounts(ctx, shareMint);
  const vaults: PublicKey[] = [];
  const quads = [];
  for (const m of mints) {
    const vault = await createAta(ctx, m, basket, TOKEN_PROGRAM_ID);
    const mine = await createAta(ctx, m, creator.publicKey, TOKEN_PROGRAM_ID);
    await mintTo(ctx, m, mine, 1_000n * ONE_A, TOKEN_PROGRAM_ID);
    vaults.push(vault);
    quads.push(
      { pubkey: vault, isSigner: false, isWritable: true },
      { pubkey: mine, isSigner: false, isWritable: true },
      { pubkey: m, isSigner: false, isWritable: false },
      { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
    );
  }
  const shares = await createAta(ctx, shareMint, creator.publicKey, TOKEN_PROGRAM_ID);
  await rpc(
    ctx,
    ctx.vault.methods
      .seedBasket(new BN((10n * ONE_SHARE).toString()), [2_600_000, 2_418_000, 2_491_000, 2_491_000].map((u) => new BN(u)))
      .accountsPartial({ payer: creator.publicKey, basket, shareMint, payerShares: shares, ...feeShares(shareMint), allowEntry: null, shareTokenProgram: TOKEN_PROGRAM_ID })
      .remainingAccounts(quads),
    [creator],
  );
  const keeper = fund(ctx);
  await rpc(ctx, ctx.vault.methods.initKeepers(keeper.publicKey, keeper.publicKey).accountsPartial({ authority: ctx.payer.publicKey, keepers: pda.keepers() }));
  const keeperAtas: PublicKey[] = [];
  for (const m of mints) {
    await rpc(
      ctx,
      ctx.vault.methods.setPrice(new BN(PRICE_A.toString())).accountsPartial({ priceAuthority: keeper.publicKey, keepers: pda.keepers(), mint: m, assetPrice: pda.price(m) }),
      [keeper],
    );
    const k = await createAta(ctx, m, keeper.publicKey, TOKEN_PROGRAM_ID);
    await mintTo(ctx, m, k, 1_000n * ONE_A, TOKEN_PROGRAM_ID);
    keeperAtas.push(k);
  }
  return { ctx, mints, vaults, basket, shareMint, keeper, keeperAtas };
}

describe("rebalance sessions", () => {
  it("a session ends when no trade it would accept is left", async () => {
    const w = await fourWorld();
    const bookAll = w.mints.flatMap((m, i) => [
      { pubkey: w.vaults[i], isSigner: false, isWritable: false },
      { pubkey: pda.price(m), isSigner: false, isWritable: false },
    ]);
    const start = await w.ctx.vault.methods
      .rebalanceStart(0, 1, new BN(730_000), 50)
      .accountsPartial({
        rebalancer: w.keeper.publicKey,
        keepers: pda.keepers(),
        basket: w.basket,
        rebalanceState: pda.rebalance(w.basket),
        sellMint: w.mints[0],
        buyMint: w.mints[1],
        sellVault: w.vaults[0],
        rebalancerSell: w.keeperAtas[0],
        sellTokenProgram: TOKEN_PROGRAM_ID,
        instructions: SYSVAR_INSTRUCTIONS_PUBKEY,
      })
      .remainingAccounts(bookAll)
      .instruction();
    const deliver = createTransferCheckedInstruction(w.keeperAtas[1], w.mints[1], w.vaults[1], w.keeper.publicKey, 730_000n, 6, [], TOKEN_PROGRAM_ID);
    const finish = await w.ctx.vault.methods
      .rebalanceFinish()
      .accountsPartial({ rebalancer: w.keeper.publicKey, keepers: pda.keepers(), basket: w.basket, shareMint: w.shareMint, rebalanceState: pda.rebalance(w.basket) })
      .remainingAccounts(bookAll)
      .instruction();
    await send(w.ctx, [start, deliver, finish], [w.keeper]);
    const st = await w.ctx.vault.account.rebalanceState.fetch(pda.rebalance(w.basket));
    assert.equal(st.sessionActive, false, "a session with nothing left to trade stays open");
  });
});

// A token the basket was sent but doesn't hold: S, a $2 token, 3 of them in the basket's account.
const PRICE_S = 2_000_000_000_000n;

async function strayWorld() {
  const w = await keeperWorld();
  const stray = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
  const strayVault = await createAta(w.ctx, stray, w.basket, TOKEN_PROGRAM_ID);
  await mintTo(w.ctx, stray, strayVault, 3n * ONE_A, TOKEN_PROGRAM_ID);
  await rpc(
    w.ctx,
    w.ctx.vault.methods.setPrice(new BN(PRICE_S.toString())).accountsPartial({ priceAuthority: w.keeper.publicKey, keepers: pda.keepers(), mint: stray, assetPrice: pda.price(stray) }),
    [w.keeper],
  );
  const keeperStray = await createAta(w.ctx, stray, w.keeper.publicKey, TOKEN_PROGRAM_ID);
  return { ...w, stray, strayVault, keeperStray };
}
type SW = Awaited<ReturnType<typeof strayWorld>>;

function sweepIx(w: SW, amount: bigint, opts: { mint?: PublicKey; vault?: PublicKey; receive?: PublicKey; program?: PublicKey } = {}) {
  return w.ctx.vault.methods
    .sweepStart(0, new BN(amount.toString()), 50)
    .accountsPartial({
      rebalancer: w.keeper.publicKey,
      keepers: pda.keepers(),
      basket: w.basket,
      rebalanceState: pda.rebalance(w.basket),
      strayMint: opts.mint ?? w.stray,
      strayVault: opts.vault ?? w.strayVault,
      strayPrice: pda.price(opts.mint ?? w.stray),
      buyMint: w.mintA,
      rebalancerSell: opts.receive ?? w.keeperStray,
      strayTokenProgram: opts.program ?? TOKEN_PROGRAM_ID,
      instructions: SYSVAR_INSTRUCTIONS_PUBKEY,
    })
    .remainingAccounts(book(w))
    .instruction();
}

describe("sweeping stray tokens", () => {
  it("a sweep swaps a token the basket was sent into one of its own, for every holder", async () => {
    const w = await strayWorld();
    const vA = balance(w.ctx, w.vaultA);
    const before = await w.ctx.vault.account.basket.fetch(w.basket);
    // 3 S at $2 = $6, delivered as 6 A.
    await send(w.ctx, [await sweepIx(w, 3n * ONE_A), deliverA(w, 6n * ONE_A), await finishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.strayVault), 0n);
    assert.equal(balance(w.ctx, w.vaultA), vA + 6n * ONE_A);
    const b = await w.ctx.vault.account.basket.fetch(w.basket);
    // 10 shares: A's units are its new balance / 10, and B's are untouched.
    assert.equal(b.constituents[0].unitsPerShare.toString(), ((vA + 6n * ONE_A) / 10n).toString());
    assert.equal(b.constituents[1].unitsPerShare.toString(), before.constituents[1].unitsPerShare.toString());
    // Not a rebalance: no session opens and the schedule doesn't move.
    const st = await w.ctx.vault.account.rebalanceState.fetch(pda.rebalance(w.basket));
    assert.equal(st.sessionActive, false);
    assert.equal(b.lastRebalance.toString(), before.lastRebalance.toString());
  });

  it("a sweep below the oracle floor reverts", async () => {
    const w = await strayWorld();
    await expectError(send(w.ctx, [await sweepIx(w, 3n * ONE_A), deliverA(w, 5_800_000n), await finishIx(w)], [w.keeper]), "BelowOracleFloor");
    assert.equal(balance(w.ctx, w.strayVault), 3n * ONE_A, "nothing left the basket");
  });

  it("a sweep needs its finish in the same transaction", async () => {
    const w = await strayWorld();
    await expectError(send(w.ctx, [await sweepIx(w, 3n * ONE_A)], [w.keeper]), "FinishMissing");
  });

  it("the basket's own tokens and its shares can't be swept", async () => {
    const w = await strayWorld();
    await expectError(
      send(w.ctx, [await sweepIx(w, ONE_A, { mint: w.mintA, vault: w.vaultA, receive: w.keeperA }), deliverA(w, ONE_A), await finishIx(w)], [w.keeper]),
      "NotStray",
    );
    const sharesHeld = await createAta(w.ctx, w.shareMint, w.basket, TOKEN_PROGRAM_ID);
    await rpc(
      w.ctx,
      w.ctx.vault.methods.setPrice(new BN(PRICE_A.toString())).accountsPartial({ priceAuthority: w.keeper.publicKey, keepers: pda.keepers(), mint: w.shareMint, assetPrice: pda.price(w.shareMint) }),
      [w.keeper],
    );
    const keeperShares = await createAta(w.ctx, w.shareMint, w.keeper.publicKey, TOKEN_PROGRAM_ID);
    await expectError(
      send(w.ctx, [await sweepIx(w, 1n, { mint: w.shareMint, vault: sharesHeld, receive: keeperShares }), deliverA(w, ONE_A), await finishIx(w)], [w.keeper]),
      "NotStray",
    );
  });
});

