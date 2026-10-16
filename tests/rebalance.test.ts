// Automatic rebalancing: the vault lends an overweight sleeve to the rebalancer, who swaps
// it on the market in the same transaction, and the vault checks it got back at least the
// oracle value (minus a small tolerance) of the underweight sleeve.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { SYSVAR_INSTRUCTIONS_PUBKEY, type Keypair, type PublicKey } from "@solana/web3.js";
import { createTransferCheckedInstruction } from "@solana/spl-token";
import { BN, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, ata, balance, createAta, expectError, fund, mintTo, now, pda, rpc, send, warp } from "./helpers.ts";
import { world, type World } from "./fixtures.ts";

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
