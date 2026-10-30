// Stop losses: a creator sets a stop on an asset; once the asset's posted price has stayed below it
// for five minutes, the asset's target goes to 0% and the others scale up to fill the basket, and
// rebalancing sells it out in clips of up to half the basket.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { SYSVAR_INSTRUCTIONS_PUBKEY, type Keypair, PublicKey } from "@solana/web3.js";
import { createTransferCheckedInstruction } from "@solana/spl-token";
import {
  BN,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  ata,
  balance,
  createAta,
  createMint,
  expectError,
  fund,
  mintTo,
  pda,
  postPrices,
  rpc,
  send,
  warp,
} from "./helpers.ts";
import { NO_GRAVI, SEED_SHARES, UNITS_A, UNITS_B, feeShares, listAsset, quads, world, type World } from "./fixtures.ts";

// World: 10 shares backed by 20 A ($1, 6 decimals) and 5 B ($6, 9 decimals): $20 + $30 = $50,
// targets 60% A / 40% B.
const E12 = 1_000_000_000_000n;
const PRICE_A = E12;
const PRICE_B = 6n * E12;
const ONE_A = 1_000_000n;
const ONE_B = 1_000_000_000n;
const CONFIRM = 300;

async function stopWorld(opts: Parameters<typeof world>[0] = {}) {
  const w = await world(opts);
  const keeper = fund(w.ctx);
  await rpc(w.ctx, w.ctx.vault.methods.initKeepers(keeper.publicKey, keeper.publicKey).accountsPartial({ authority: w.ctx.payer.publicKey, keepers: pda.keepers() }));
  const keeperA = await createAta(w.ctx, w.mintA, keeper.publicKey, TOKEN_PROGRAM_ID);
  await mintTo(w.ctx, w.mintA, keeperA, 1_000n * ONE_A, TOKEN_PROGRAM_ID);
  const keeperB = await createAta(w.ctx, w.mintB, keeper.publicKey, TOKEN_2022_PROGRAM_ID);
  const stopLoss = PublicKey.findProgramAddressSync([Buffer.from("stop"), w.basket.toBuffer()], w.ctx.vault.programId)[0];
  const sw = { ...w, keeper, keeperA, keeperB, stopLoss };
  await prices(sw, PRICE_A, PRICE_B);
  return sw;
}
type SW = Awaited<ReturnType<typeof stopWorld>>;

/** Posts both prices (stepping within the 20%-an-hour band if needed), fresh together. */
const prices = (w: SW, a: bigint, b: bigint) => postPrices(w.ctx, w.keeper, [[w.mintA, a], [w.mintB, b]]);

const stopArgs = (mint: PublicKey, pctBps: number, referenceE12: bigint, trailing = false) => ({ mint, pctBps, trailing, referenceE12: new BN(referenceE12.toString()) });

const setStops = (w: SW, stops: ReturnType<typeof stopArgs>[], signer: Keypair = w.creator) =>
  rpc(w.ctx, w.ctx.vault.methods.setStops(stops).accountsPartial({ creator: signer.publicKey, basket: w.basket, stopLoss: w.stopLoss }), [signer]);

const checkStop = (w: SW, mint: PublicKey) =>
  rpc(w.ctx, w.ctx.vault.methods.checkStop().accountsPartial({ basket: w.basket, stopLoss: w.stopLoss, assetPrice: pda.price(mint) }));

const targets = async (w: SW) => ((await w.ctx.vault.account.basket.fetch(w.basket)).constituents as { targetBps: number }[]).map((c) => c.targetBps);
const stops = async (w: SW) => (await w.ctx.vault.account.stopLoss.fetch(w.stopLoss)) as unknown as { stops: { mint: PublicKey; armedAt: BN; referenceE12: BN }[]; fired: number };

/** One rebalance slice selling `amountB` of B for `getA` of A, in one transaction. */
async function sellB(w: SW, amountB: bigint, getA: bigint) {
  const book = [
    { pubkey: w.vaultA, isSigner: false, isWritable: false },
    { pubkey: pda.price(w.mintA), isSigner: false, isWritable: false },
    { pubkey: w.vaultB, isSigner: false, isWritable: false },
    { pubkey: pda.price(w.mintB), isSigner: false, isWritable: false },
  ];
  const start = await w.ctx.vault.methods
    .rebalanceStart(1, 0, new BN(amountB.toString()), 50, new BN(0))
    .accountsPartial({
      rebalancer: w.keeper.publicKey,
      keepers: pda.keepers(),
      basket: w.basket,
      rebalanceState: pda.rebalance(w.basket),
      sellMint: w.mintB,
      buyMint: w.mintA,
      sellVault: w.vaultB,
      rebalancerSell: w.keeperB,
      sellTokenProgram: TOKEN_2022_PROGRAM_ID,
      instructions: SYSVAR_INSTRUCTIONS_PUBKEY,
    })
    .remainingAccounts(book)
    .instruction();
  const deliver = createTransferCheckedInstruction(w.keeperA, w.mintA, w.vaultA, w.keeper.publicKey, getA, 6, [], TOKEN_PROGRAM_ID);
  const finish = await w.ctx.vault.methods
    .rebalanceFinish()
    .accountsPartial({ rebalancer: w.keeper.publicKey, keepers: pda.keepers(), basket: w.basket, shareMint: w.shareMint, rebalanceState: pda.rebalance(w.basket) })
    .remainingAccounts(book)
    .instruction();
  await send(w.ctx, [start, deliver, finish], [w.keeper]);
}

/** Arms B's stop at `price` and fires it CONFIRM seconds later, the price posted fresh each time. */
async function fireB(w: SW, price: bigint) {
  await prices(w, PRICE_A, price);
  await checkStop(w, w.mintB);
  warp(w.ctx, CONFIRM);
  await prices(w, PRICE_A, price);
  await checkStop(w, w.mintB);
}

async function seed(w: World) {
  const holder = ata(w.shareMint, w.creator.publicKey, w.shareProgram);
  await rpc(
    w.ctx,
    w.ctx.vault.methods
      .seedBasket(new BN(SEED_SHARES.toString()), [new BN(UNITS_A.toString()), new BN(UNITS_B.toString())])
      .accountsPartial({ payer: w.creator.publicKey, basket: w.basket, shareMint: w.shareMint, payerShares: holder, ...feeShares(w.shareMint, w.shareProgram), shareTokenProgram: w.shareProgram })
      .remainingAccounts(quads(w, w.creator.publicKey)),
    [w.creator],
  );
}

describe("stop losses", () => {
  it("the creator sets stops; nobody else can, and bad ones are refused", async () => {
    const w = await stopWorld();
    await expectError(setStops(w, [stopArgs(w.mintB, 3_000, PRICE_B)], fund(w.ctx)), "NotMandateAuthority");
    await expectError(setStops(w, [stopArgs(w.mintB, 400, PRICE_B)]), "BadStop");
    await expectError(setStops(w, [stopArgs(w.mintB, 9_100, PRICE_B)]), "BadStop");
    await expectError(setStops(w, [stopArgs(w.mintB, 3_000, 0n)]), "BadStop");
    await expectError(setStops(w, [stopArgs(w.mintB, 3_000, PRICE_B), stopArgs(w.mintB, 2_000, PRICE_B)]), "BadStop");
    await expectError(setStops(w, [stopArgs(await createMint(w.ctx, 6, TOKEN_PROGRAM_ID), 3_000, PRICE_B)]), "BadStop");
    await setStops(w, [stopArgs(w.mintA, 5_000, PRICE_A), stopArgs(w.mintB, 3_000, PRICE_B)]);
    assert.equal((await stops(w)).stops.length, 2);
    await setStops(w, []);
    assert.equal((await stops(w)).stops.length, 0);
  });

  it("a stop arms below its level, fires five minutes later, and the rest of the basket scales up", async () => {
    const w = await stopWorld();
    await setStops(w, [stopArgs(w.mintB, 3_000, PRICE_B)]); // B's stop: $4.20
    // At $4.50, above the stop: nothing.
    await prices(w, PRICE_A, 4_500_000_000_000n);
    await checkStop(w, w.mintB);
    assert.equal(Number((await stops(w)).stops[0].armedAt), 0);
    // At $4.10: armed, targets unchanged.
    await prices(w, PRICE_A, 4_100_000_000_000n);
    await checkStop(w, w.mintB);
    assert.ok(Number((await stops(w)).stops[0].armedAt) > 0);
    assert.deepEqual(await targets(w), [6_000, 4_000]);
    // A check a minute short of the five changes nothing.
    warp(w.ctx, CONFIRM - 60);
    await prices(w, PRICE_A, 4_100_000_000_000n);
    await checkStop(w, w.mintB);
    assert.deepEqual(await targets(w), [6_000, 4_000]);
    // Five minutes after arming: fired. B goes to 0%, A to 100%, and the stop is gone.
    warp(w.ctx, 60);
    await prices(w, PRICE_A, 4_100_000_000_000n);
    await checkStop(w, w.mintB);
    assert.deepEqual(await targets(w), [10_000, 0]);
    const s = await stops(w);
    assert.equal(s.stops.length, 0);
    assert.equal(s.fired, 1);
    await expectError(checkStop(w, w.mintB), "StopNotSet");
  });

  it("a price back above the stop disarms it, and arming starts over", async () => {
    const w = await stopWorld();
    await setStops(w, [stopArgs(w.mintB, 3_000, PRICE_B)]);
    await prices(w, PRICE_A, 4_100_000_000_000n);
    await checkStop(w, w.mintB);
    warp(w.ctx, 120);
    await prices(w, PRICE_A, 4_300_000_000_000n);
    await checkStop(w, w.mintB);
    assert.equal(Number((await stops(w)).stops[0].armedAt), 0);
    // Below again 4 minutes later: armed anew, so not yet fired.
    warp(w.ctx, 240);
    await prices(w, PRICE_A, 4_100_000_000_000n);
    await checkStop(w, w.mintB);
    assert.deepEqual(await targets(w), [6_000, 4_000]);
  });

  it("a stale price neither arms nor fires a stop", async () => {
    const w = await stopWorld();
    await setStops(w, [stopArgs(w.mintB, 3_000, PRICE_B)]);
    await prices(w, PRICE_A, 4_100_000_000_000n);
    warp(w.ctx, 121);
    await expectError(checkStop(w, w.mintB), "BadPrice");
  });

  it("a trailing stop's reference rises with the price and never falls", async () => {
    const w = await stopWorld();
    await setStops(w, [stopArgs(w.mintB, 3_000, PRICE_B, true), stopArgs(w.mintA, 3_000, PRICE_A)]);
    await prices(w, PRICE_A, 8n * E12);
    await checkStop(w, w.mintB);
    assert.equal(BigInt((await stops(w)).stops[0].referenceE12.toString()), 8n * E12);
    await prices(w, PRICE_A, 7n * E12);
    await checkStop(w, w.mintB);
    assert.equal(BigInt((await stops(w)).stops[0].referenceE12.toString()), 8n * E12);
    // $5.50 is under 70% of $8 (a fixed stop from $6 would sit at $4.20): armed.
    await prices(w, PRICE_A, 5_500_000_000_000n);
    await checkStop(w, w.mintB);
    assert.ok(Number((await stops(w)).stops[0].armedAt) > 0);
    // A's stop is fixed: a higher price leaves its reference alone.
    await prices(w, 1_100_000_000_000n, 5_500_000_000_000n);
    await checkStop(w, w.mintA);
    assert.equal(BigInt((await stops(w)).stops[1].referenceE12.toString()), PRICE_A);
  });

  it("the stopped asset sells out in clips of up to half the basket, other slices stay at 5%", async () => {
    const w = await stopWorld();
    await setStops(w, [stopArgs(w.mintB, 3_000, PRICE_B)]);
    await fireB(w, 4n * E12); // B now $20 of $40: 50%, target 0
    assert.deepEqual(await targets(w), [10_000, 0]);
    // Half the basket is $20 = all 5 B: one clip may take 4 B ($16, 40%), more than 5% ever could.
    await prices(w, PRICE_A, 4n * E12);
    await sellB(w, 4n * ONE_B, 16n * ONE_A);
    assert.equal(balance(w.ctx, w.vaultB), ONE_B);
    // The last B ($4, 10% of $40) leaves in the next clip.
    await sellB(w, ONE_B, 4n * ONE_A);
    assert.equal(balance(w.ctx, w.vaultB), 0n);
  });

  it("a clip can't pass half the basket", async () => {
    // B is 60% at its $6 price once set to 0%; half the basket ($25) is 4.1666 B.
    const w = await stopWorld();
    await rpc(w.ctx, w.ctx.vault.methods.setTargets([10_000, 0]).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }), [w.creator]);
    await prices(w, PRICE_A, PRICE_B);
    await expectError(sellB(w, 4_250_000_000n, 25_500_000n), "SliceTooLarge");
    await sellB(w, 4_160_000_000n, 24_960_000n);
  });

  it("the last asset with a target can't be stopped out", async () => {
    const w = await stopWorld();
    await rpc(w.ctx, w.ctx.vault.methods.setTargets([10_000, 0]).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }), [w.creator]);
    await setStops(w, [stopArgs(w.mintA, 3_000, PRICE_A), stopArgs(w.mintB, 3_000, PRICE_B)]);
    // B is already at 0%: its stop has nothing to do.
    await prices(w, PRICE_A, 4n * E12);
    await expectError(checkStop(w, w.mintB), "StopInactive");
    await prices(w, 500_000_000_000n, PRICE_B);
    await checkStop(w, w.mintA);
    warp(w.ctx, CONFIRM);
    await prices(w, 500_000_000_000n, PRICE_B);
    await expectError(checkStop(w, w.mintA), "StopLastAsset");
  });

  it("a 0% asset whose price stopped a day ago counts as $0, so the basket isn't stuck", async () => {
    const w = await stopWorld();
    await setStops(w, [stopArgs(w.mintB, 3_000, PRICE_B)]);
    await fireB(w, 4n * E12);
    // B's price is never posted again; a day on, only A's is.
    warp(w.ctx, 86_401);
    await postPrices(w.ctx, w.keeper, [[w.mintA, PRICE_A]]);
    // The books value B at nothing instead of refusing it: its sale now fails for having no
    // floor, not for a stale price.
    await expectError(sellB(w, ONE_B, 4n * ONE_A), "SliceTooSmall");
  });

  it("immutable baskets: stops only before the first deposit, never changed or closed, and they fire", async () => {
    const w = await stopWorld({ control: "immutable", seed: false });
    await setStops(w, [stopArgs(w.mintB, 3_000, PRICE_B)]);
    await seed(w);
    await expectError(setStops(w, []), "StopsFixed");
    await expectError(rpc(w.ctx, w.ctx.vault.methods.closeStops().accountsPartial({ creator: w.creator.publicKey, basket: w.basket, stopLoss: w.stopLoss }), [w.creator]), "BasketImmutable");
    await fireB(w, 4n * E12);
    assert.deepEqual(await targets(w), [10_000, 0]);
  });

  it("a creator-managed basket's creator can close its stops, rent back", async () => {
    const w = await stopWorld();
    await setStops(w, [stopArgs(w.mintB, 3_000, PRICE_B)]);
    const before = w.ctx.svm.getBalance(w.creator.publicKey)!;
    await rpc(w.ctx, w.ctx.vault.methods.closeStops().accountsPartial({ creator: w.creator.publicKey, basket: w.basket, stopLoss: w.stopLoss }), [w.creator]);
    assert.ok(w.ctx.svm.getBalance(w.creator.publicKey)! > before);
    assert.equal(w.ctx.svm.getAccount(w.stopLoss), null);
  });

  it("firing splits the stopped weight across three assets, the rounding to the largest", async () => {
    const w = await stopWorld();
    const mintC = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
    await listAsset(w.ctx, mintC);
    const vaultC = await createAta(w.ctx, mintC, w.basket, TOKEN_PROGRAM_ID);
    const add = await w.ctx.vault.methods
      .addConstituent()
      .accountsPartial({ ...NO_GRAVI, authority: w.creator.publicKey, basket: w.basket, mint: mintC, assetEntry: pda.asset(mintC), vault: vaultC })
      .instruction();
    const set = await w.ctx.vault.methods.setTargets([3_000, 3_000, 4_000]).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }).instruction();
    await send(w.ctx, [add, set], [w.creator]);
    await setStops(w, [stopArgs(w.mintB, 3_000, PRICE_B)]);
    await fireB(w, 4n * E12);
    // 3000 / 7000 = 4285.7, 4000 / 7000 = 5714.3: 4285 + 5714 = 9999, the 1 to C.
    assert.deepEqual(await targets(w), [4_285, 0, 5_715]);
  });
});
