// Tokens that charge a Token-2022 transfer fee (stonkfun tokens: 1-3%, PreStocks: 1%). The
// payer bears the fee on a deposit, the holder on a redemption, and rebalancing allows for it
// in the oracle floor. A fee above 3% can't be listed, and a fee raised above 3% after listing
// stops new money while redemptions go on.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { SYSVAR_INSTRUCTIONS_PUBKEY, type Keypair, type PublicKey } from "@solana/web3.js";
import { createSetTransferFeeInstruction, createTransferCheckedInstruction } from "@solana/spl-token";
import { Clock } from "litesvm";
import { BN, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, ata, balance, createAta, createMint, expectError, fund, mintTo, pda, postPrices, rpc, send } from "./helpers.ts";
import { SEED_SHARES, UNITS_A, UNITS_B, fundHolder, listAsset, mintIx, redeemIx, world, type World } from "./fixtures.ts";

const FEE_BPS = 100; // 1%, as most stonkfun tokens and PreStocks
const ONE_SHARE = 1_000_000_000n;
const feeOn = (amount: bigint, bps = FEE_BPS) => (amount * BigInt(bps) + 9_999n) / 10_000n;

/** Moves the clock `n` epochs on: a new transfer fee takes effect two epochs after it's set. */
function warpEpochs(w: World, n: number) {
  const c = w.ctx.svm.getClock();
  w.ctx.svm.setClock(new Clock(c.slot + 1n, c.epochStartTimestamp, c.epoch + BigInt(n), c.leaderScheduleEpoch, c.unixTimestamp + 60n));
  w.ctx.svm.expireBlockhash();
}

/** The fee authority (the test payer) sets token B's fee. */
async function setFee(w: World, bps: number) {
  await send(w.ctx, [createSetTransferFeeInstruction(w.mintB, w.ctx.payer.publicKey, [], bps, 2n ** 63n, TOKEN_2022_PROGRAM_ID)]);
  warpEpochs(w, 2);
}

describe("tokens that charge a transfer fee", () => {
  it("seeding a basket that holds one: the vault gets every unit, the creator pays the fee", async () => {
    const w = await world({ feeBpsB: FEE_BPS });
    const wanted = (SEED_SHARES * UNITS_B) / ONE_SHARE;
    assert.equal(balance(w.ctx, w.vaultB), wanted, "the vault holds exactly the units it promised");
    assert.equal(balance(w.ctx, w.vaultA), (SEED_SHARES * UNITS_A) / ONE_SHARE);
  });

  it("minting: the buyer sends the units plus the fee, and the vault is credited the units", async () => {
    const w = await world({ feeBpsB: FEE_BPS });
    const alice = fund(w.ctx);
    const h = await fundHolder(w, alice);
    const [vB, aB] = [balance(w.ctx, w.vaultB), balance(w.ctx, h.b)];
    await rpc(w.ctx, mintIx(w, alice, ONE_SHARE), [alice]);
    const units = UNITS_B;
    assert.equal(balance(w.ctx, w.vaultB) - vB, units, "the vault got the full slice");
    const paid = aB - balance(w.ctx, h.b);
    // The fee is charged on what's sent, so delivering `units` takes units / 0.99.
    const gross = (units * 10_000n + 9_899n) / 9_900n;
    assert.ok(paid > units && paid <= gross + 1n, `paid ${paid}, expected about ${gross}`);
    assert.equal(balance(w.ctx, h.shares), ONE_SHARE);
  });

  it("a buyer's balance covers the shares it can deliver after the fee", async () => {
    const w = await world({ feeBpsB: FEE_BPS });
    const alice = fund(w.ctx);
    const a = await createAta(w.ctx, w.mintA, alice.publicKey, TOKEN_PROGRAM_ID);
    const b = await createAta(w.ctx, w.mintB, alice.publicKey, TOKEN_2022_PROGRAM_ID);
    const shares = await createAta(w.ctx, w.shareMint, alice.publicKey);
    await mintTo(w.ctx, w.mintA, a, 10n * UNITS_A, TOKEN_PROGRAM_ID);
    // Exactly one share's worth of B before the fee: after it, 0.99 of a share.
    await mintTo(w.ctx, w.mintB, b, UNITS_B, TOKEN_2022_PROGRAM_ID);
    await rpc(w.ctx, mintIx(w, alice, ONE_SHARE, null, 1n), [alice]);
    const got = balance(w.ctx, shares);
    const covered = ((UNITS_B - feeOn(UNITS_B)) * ONE_SHARE) / UNITS_B;
    assert.ok(got > 0n && got <= covered && got >= covered - 2n, `minted ${got}, covered ${covered}`);
  });

  it("redeeming pays out the units; the holder receives them less the fee", async () => {
    const w = await world({ feeBpsB: FEE_BPS });
    const holder = ata(w.mintB, w.creator.publicKey, TOKEN_2022_PROGRAM_ID);
    const [vB, hB] = [balance(w.ctx, w.vaultB), balance(w.ctx, holder)];
    await rpc(w.ctx, redeemIx(w, w.creator, ONE_SHARE), [w.creator]);
    assert.equal(vB - balance(w.ctx, w.vaultB), UNITS_B, "the vault paid the full slice");
    assert.equal(balance(w.ctx, holder) - hB, UNITS_B - feeOn(UNITS_B), "the holder got it less 1%");
  });

  it("a token charging more than 3% can't be listed", async () => {
    const w = await world();
    const steep = await createMint(w.ctx, 6, TOKEN_2022_PROGRAM_ID, undefined, 400);
    await expectError(listAsset(w.ctx, steep), "TransferFeeTooHigh");
    const fine = await createMint(w.ctx, 6, TOKEN_2022_PROGRAM_ID, undefined, 300);
    await listAsset(w.ctx, fine);
  });

  it("a fee raised above 3% after listing stops new money, and holders can still redeem", async () => {
    const w = await world({ feeBpsB: FEE_BPS });
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await setFee(w, 500);
    await expectError(rpc(w.ctx, mintIx(w, alice, ONE_SHARE), [alice]), "TransferFeeTooHigh");
    const holder = ata(w.mintB, w.creator.publicKey, TOKEN_2022_PROGRAM_ID);
    const hB = balance(w.ctx, holder);
    await rpc(w.ctx, redeemIx(w, w.creator, ONE_SHARE), [w.creator]);
    assert.equal(balance(w.ctx, holder) - hB, UNITS_B - feeOn(UNITS_B, 500), "redeemed, less the new 5% fee");
  });
});

// Rebalancing a basket of A ($1, no fee) and B ($6, 1% fee): 10 shares hold 20 A and 5 B,
// $20 / $30 against targets of 60 / 40, so B is sold for A.
const PRICE_A = 1_000_000_000_000n;
const PRICE_B = 6_000_000_000_000n;
const SLICE_B = 416_666_666n; // $2.50 of B
const FAIR_A = 2_500_000n; // its oracle value in A

async function keeperWorld() {
  const w = await world({ feeBpsB: FEE_BPS });
  const keeper = fund(w.ctx);
  await rpc(
    w.ctx,
    w.ctx.vault.methods.initKeepers(keeper.publicKey, keeper.publicKey).accountsPartial({ authority: w.ctx.payer.publicKey, keepers: pda.keepers() }),
  );
  await setPrices(w, keeper, PRICE_A, PRICE_B);
  const keeperA = await createAta(w.ctx, w.mintA, keeper.publicKey, TOKEN_PROGRAM_ID);
  await mintTo(w.ctx, w.mintA, keeperA, 1_000_000_000n, TOKEN_PROGRAM_ID);
  const keeperB = await createAta(w.ctx, w.mintB, keeper.publicKey, TOKEN_2022_PROGRAM_ID);
  return { ...w, keeper, keeperA, keeperB };
}
type KW = Awaited<ReturnType<typeof keeperWorld>>;

async function setPrices(w: World, keeper: Keypair, a: bigint, b: bigint) {
  await postPrices(w.ctx, keeper, [[w.mintA, a], [w.mintB, b]]);
}

const book = (w: KW) =>
  [w.vaultA, pda.price(w.mintA), w.vaultB, pda.price(w.mintB)].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false }));

function startIx(w: KW, sell: 0 | 1, amount: bigint) {
  const [sellMint, buyMint, sellVault, receive, program] =
    sell === 1 ? [w.mintB, w.mintA, w.vaultB, w.keeperB, TOKEN_2022_PROGRAM_ID] : [w.mintA, w.mintB, w.vaultA, w.keeperA, TOKEN_PROGRAM_ID];
  return w.ctx.vault.methods
    .rebalanceStart(sell, 1 - sell, new BN(amount.toString()), 50)
    .accountsPartial({
      rebalancer: w.keeper.publicKey,
      keepers: pda.keepers(),
      basket: w.basket,
      rebalanceState: pda.rebalance(w.basket),
      sellMint,
      buyMint,
      sellVault,
      rebalancerSell: receive,
      sellTokenProgram: program,
      instructions: SYSVAR_INSTRUCTIONS_PUBKEY,
    })
    .remainingAccounts(book(w))
    .instruction();
}

const finishIx = (w: KW) =>
  w.ctx.vault.methods
    .rebalanceFinish()
    .accountsPartial({ rebalancer: w.keeper.publicKey, keepers: pda.keepers(), basket: w.basket, shareMint: w.shareMint, rebalanceState: pda.rebalance(w.basket) })
    .remainingAccounts(book(w))
    .instruction();

const deliverA = (w: KW, amount: bigint, to: PublicKey) =>
  createTransferCheckedInstruction(w.keeperA, w.mintA, to, w.keeper.publicKey, amount, 6, [], TOKEN_PROGRAM_ID);

describe("rebalancing a token that charges a transfer fee", () => {
  it("the floor allows for the fee: selling it at a fair price after the fee clears", async () => {
    const w = await keeperWorld();
    // The keeper receives the slice less 1%, so a fair swap of what arrived brings back 1% less.
    const afterFee = (FAIR_A * 99n) / 100n;
    await send(w.ctx, [await startIx(w, 1, SLICE_B), deliverA(w, afterFee, w.vaultA), await finishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.keeperB), SLICE_B - feeOn(SLICE_B), "the lent slice arrived less the fee");
  });

  it("but not for more than the fee: a bad fill on top of it still reverts", async () => {
    const w = await keeperWorld();
    const vB = balance(w.ctx, w.vaultB);
    // 2% short: the 1% fee plus a 1% worse price, beyond the 0.5% tolerance.
    await expectError(send(w.ctx, [await startIx(w, 1, SLICE_B), deliverA(w, (FAIR_A * 98n) / 100n, w.vaultA), await finishIx(w)], [w.keeper]), "BelowOracleFloor");
    assert.equal(balance(w.ctx, w.vaultB), vB, "nothing left the vault");
  });

  it("no rebalance buys into a token whose fee was raised above 3%", async () => {
    const w = await keeperWorld();
    await setFee(w, 400);
    // B falls to $2, so it's the underweight sleeve: $20 of A, $10 of B.
    await setPrices(w, w.keeper, PRICE_A, 2_000_000_000_000n);
    await expectError(send(w.ctx, [await startIx(w, 0, 1_000_000n), await finishIx(w)], [w.keeper]), "TransferFeeTooHigh");
  });
});
