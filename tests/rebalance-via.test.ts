// A slice through SOL (rebalance_start_via): for a pair no single route fits a transaction, the
// overweight sleeve is sold into the basket's own wrapped SOL sleeve, then an ordinary slice sells
// that SOL into the sleeve under target. The SOL stays in the basket's vault and in its books.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { SYSVAR_INSTRUCTIONS_PUBKEY, SystemProgram, PublicKey } from "@solana/web3.js";
import { MINT_SIZE, MintLayout, NATIVE_MINT, createSyncNativeInstruction, createTransferCheckedInstruction } from "@solana/spl-token";
import { BN, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, balance, createAta, createMint, expectError, fund, mintTo, pda, rpc, send, postPrices } from "./helpers.ts";
import { NO_GRAVI, listAsset, world } from "./fixtures.ts";

// World: 10 shares backed by 20 A ($1) and 5 B ($6): $20 / $30 against targets of 60 / 40, and a
// SOL sleeve ($100) at 0%, empty. B must be sold for A.
const PRICE_A = 1_000_000_000_000n;
const PRICE_B = 6_000_000_000_000n;
const PRICE_SOL = 100_000_000_000_000n;
const ONE_A = 1_000_000n;
/** $2.4999 of B: the 5% slice cap on a $50 basket. */
const SLICE_B = 416_666_666n;
/** The same $2.50 in SOL at $100, and in A at $1. */
const FAIR_SOL = 25_000_000n;
const FAIR_A = 2_500_000n;

/** `targets`: A, B, SOL (and C, a $1 token, when given four). */
async function hubWorld(targets = [6_000, 4_000, 0]) {
  const w = await world();
  const keeper = fund(w.ctx);
  await rpc(w.ctx, w.ctx.vault.methods.initKeepers(keeper.publicKey, keeper.publicKey).accountsPartial({ authority: w.ctx.payer.publicKey, keepers: pda.keepers() }));
  // LiteSVM has no native mint account: SOL's, as on chain (9 decimals, no authority).
  const data = Buffer.alloc(MINT_SIZE);
  MintLayout.encode({ mintAuthorityOption: 0, mintAuthority: PublicKey.default, supply: 0n, decimals: 9, isInitialized: true, freezeAuthorityOption: 0, freezeAuthority: PublicKey.default }, data);
  w.ctx.svm.setAccount(NATIVE_MINT, { lamports: Number(w.ctx.svm.minimumBalanceForRentExemption(BigInt(MINT_SIZE))), data, owner: TOKEN_PROGRAM_ID, executable: false });
  await listAsset(w.ctx, NATIVE_MINT);
  const vaultSol = await createAta(w.ctx, NATIVE_MINT, w.basket, TOKEN_PROGRAM_ID);
  await rpc(
    w.ctx,
    w.ctx.vault.methods.addConstituent().accountsPartial({ ...NO_GRAVI, authority: w.creator.publicKey, basket: w.basket, mint: NATIVE_MINT, assetEntry: pda.asset(NATIVE_MINT), vault: vaultSol }),
    [w.creator],
  );
  const mintC = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
  const vaultC = await createAta(w.ctx, mintC, w.basket, TOKEN_PROGRAM_ID);
  if (targets.length === 4) {
    await listAsset(w.ctx, mintC);
    await rpc(
      w.ctx,
      w.ctx.vault.methods.addConstituent().accountsPartial({ ...NO_GRAVI, authority: w.creator.publicKey, basket: w.basket, mint: mintC, assetEntry: pda.asset(mintC), vault: vaultC }),
      [w.creator],
    );
  }
  await rpc(w.ctx, w.ctx.vault.methods.setTargets(targets).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }), [w.creator]);
  await postPrices(w.ctx, keeper, [[w.mintA, PRICE_A], [w.mintB, PRICE_B], [NATIVE_MINT, PRICE_SOL], ...(targets.length === 4 ? ([[mintC, PRICE_A]] as [PublicKey, bigint][]) : [])]);
  // The keeper is the market: it holds A and SOL to deliver, and accounts to receive B and SOL.
  const keeperA = await createAta(w.ctx, w.mintA, keeper.publicKey, TOKEN_PROGRAM_ID);
  await mintTo(w.ctx, w.mintA, keeperA, 1_000n * ONE_A, TOKEN_PROGRAM_ID);
  const keeperB = await createAta(w.ctx, w.mintB, keeper.publicKey, TOKEN_2022_PROGRAM_ID);
  const keeperSol = await createAta(w.ctx, NATIVE_MINT, keeper.publicKey, TOKEN_PROGRAM_ID);
  await send(w.ctx, [SystemProgram.transfer({ fromPubkey: keeper.publicKey, toPubkey: keeperSol, lamports: 10_000_000_000 }), createSyncNativeInstruction(keeperSol)], [keeper]);
  return { ...w, keeper, keeperA, keeperB, keeperSol, vaultSol, mintC, vaultC, four: targets.length === 4 };
}
type HW = Awaited<ReturnType<typeof hubWorld>>;

const book = (w: HW) =>
  [
    [w.vaultA, w.mintA],
    [w.vaultB, w.mintB],
    [w.vaultSol, NATIVE_MINT],
    ...(w.four ? [[w.vaultC, w.mintC]] : []),
  ].flatMap(([vault, mint]) => [
    { pubkey: vault, isSigner: false, isWritable: false },
    { pubkey: pda.price(mint), isSigner: false, isWritable: false },
  ]);

const sleeves = (w: HW) => [
  { mint: w.mintA, vault: w.vaultA, program: TOKEN_PROGRAM_ID, keeper: w.keeperA },
  { mint: w.mintB, vault: w.vaultB, program: TOKEN_2022_PROGRAM_ID, keeper: w.keeperB },
  { mint: NATIVE_MINT, vault: w.vaultSol, program: TOKEN_PROGRAM_ID, keeper: w.keeperSol },
];

function startIx(w: HW, sell: number, buy: number, amount: bigint, via: boolean) {
  const s = sleeves(w)[sell];
  const method = via ? w.ctx.vault.methods.rebalanceStartVia : w.ctx.vault.methods.rebalanceStart;
  return method(sell, buy, new BN(amount.toString()), 50, new BN(0))
    .accountsPartial({
      rebalancer: w.keeper.publicKey,
      keepers: pda.keepers(),
      basket: w.basket,
      rebalanceState: pda.rebalance(w.basket),
      sellMint: s.mint,
      buyMint: sleeves(w)[buy].mint,
      sellVault: s.vault,
      rebalancerSell: s.keeper,
      sellTokenProgram: s.program,
      instructions: SYSVAR_INSTRUCTIONS_PUBKEY,
    })
    .remainingAccounts(book(w))
    .instruction();
}
const finishIx = (w: HW) =>
  w.ctx.vault.methods
    .rebalanceFinish()
    .accountsPartial({ rebalancer: w.keeper.publicKey, keepers: pda.keepers(), basket: w.basket, shareMint: w.shareMint, rebalanceState: pda.rebalance(w.basket) })
    .remainingAccounts(book(w))
    .instruction();
/** The "market": the keeper delivers SOL or A into the basket's vault. */
const deliverSol = (w: HW, lamports: bigint) => createTransferCheckedInstruction(w.keeperSol, NATIVE_MINT, w.vaultSol, w.keeper.publicKey, lamports, 9, [], TOKEN_PROGRAM_ID);
const deliverA = (w: HW, amount: bigint) => createTransferCheckedInstruction(w.keeperA, w.mintA, w.vaultA, w.keeper.publicKey, amount, 6, [], TOKEN_PROGRAM_ID);

describe("a slice through SOL", () => {
  it("sells the overweight sleeve into the basket's SOL, then an ordinary slice sells the SOL into the sleeve under target", async () => {
    const w = await hubWorld();
    const bBefore = balance(w.ctx, w.vaultB);
    await send(w.ctx, [await startIx(w, 1, 2, SLICE_B, true), deliverSol(w, FAIR_SOL), await finishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.vaultB), bBefore - SLICE_B, "B sold");
    assert.equal(balance(w.ctx, w.vaultSol), FAIR_SOL, "its SOL is in the basket's own vault");
    const units = (await w.ctx.vault.account.basket.fetch(w.basket)).constituents[2].unitsPerShare;
    assert.ok(units.gt(new BN(0)), "and in its books: the SOL sleeve's units count it");

    // The SOL sleeve is now over its 0% target: an ordinary slice sells it for A.
    const aBefore = balance(w.ctx, w.vaultA);
    await send(w.ctx, [await startIx(w, 2, 0, FAIR_SOL, false), deliverA(w, FAIR_A), await finishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.vaultSol), 0n, "the SOL went on");
    assert.equal(balance(w.ctx, w.vaultA), aBefore + FAIR_A, "into A, the sleeve under target");
  });

  it("must buy the basket's SOL", async () => {
    const w = await hubWorld();
    await expectError(send(w.ctx, [await startIx(w, 1, 0, SLICE_B, true), deliverA(w, FAIR_A), await finishIx(w)], [w.keeper]), "NotSolHub");
  });

  it("goes nowhere unless another sleeve is under target", async () => {
    // A at its 40% target, B over at 60% against 50, SOL under at 0% against 10: an ordinary
    // slice may buy SOL, but there's nothing for SOL to go on to.
    const w = await hubWorld([4_000, 5_000, 1_000]);
    await expectError(send(w.ctx, [await startIx(w, 1, 2, 83_333_333n, true), deliverSol(w, 5_000_000n), await finishIx(w)], [w.keeper]), "WrongDirection");
  });

  it("is held to the oracle floor like any slice", async () => {
    const w = await hubWorld();
    await expectError(send(w.ctx, [await startIx(w, 1, 2, SLICE_B, true), deliverSol(w, (FAIR_SOL * 97n) / 100n), await finishIx(w)], [w.keeper]), "BelowOracleFloor");
  });

  it("can't take the SOL past what the most underweight sleeve needs", async () => {
    // A and C each 10% under target, B 20% over. Two slices through SOL put 10% in SOL: then
    // SOL is over by all A (or C) needs, and a third is refused though B is still 10% over.
    const w = await hubWorld([5_000, 4_000, 0, 1_000]);
    for (let i = 0; i < 2; i++) await send(w.ctx, [await startIx(w, 1, 2, SLICE_B, true), deliverSol(w, FAIR_SOL), await finishIx(w)], [w.keeper]);
    await expectError(send(w.ctx, [await startIx(w, 1, 2, SLICE_B, true), deliverSol(w, FAIR_SOL), await finishIx(w)], [w.keeper]), "WrongDirection");
    // An ordinary slice of B into A is still allowed: B is over, A under.
    await send(w.ctx, [await startIx(w, 1, 0, SLICE_B, false), deliverA(w, FAIR_A), await finishIx(w)], [w.keeper]);
  });
});
