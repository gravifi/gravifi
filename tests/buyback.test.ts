// The GRAVI buyback-and-burn: burn-earmarked shares are redeemed into the burn PDA's own
// accounts, lent to the rebalancer a slice at a time to swap for GRAVI in the same
// transaction, and every GRAVI that comes back is burned once it clears the oracle floor.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { Keypair, PublicKey, SYSVAR_INSTRUCTIONS_PUBKEY, SystemProgram } from "@solana/web3.js";
import { MINT_SIZE, MintLayout, NATIVE_MINT, createInitializeAccount3Instruction, createSyncNativeInstruction, createTransferCheckedInstruction } from "@solana/spl-token";
import {
  BN,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  VAULT_ID,
  ata,
  balance,
  createAta,
  expectError,
  fakeProgramData,
  fund,
  mintTo,
  pda,
  rpc,
  send,
  setup,
  supply,
} from "./helpers.ts";
import { feeShares, fundHolder, mintIx, world } from "./fixtures.ts";

const PRICE_A = 1_000_000_000_000n; // $1.00
const PRICE_B = 6_000_000_000_000n; // $6.00
const PRICE_GRAVI = 10_000_000_000n; // $0.01
const ONE_GRAVI = 1_000_000_000n;

async function buybackWorld() {
  // A 0.40% fee, half of it to the burn PDA: Alice minting 100 shares leaves 0.2 there.
  // (Seeded with no fee, then 0.40%, so the burn PDA holds exactly Alice's share of it.)
  const w = await world({ protocolFeeBps: 0 });
  await rpc(w.ctx, w.ctx.vault.methods.setProtocolFees(40, 5_000).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol() }));
  const keeper = fund(w.ctx);
  await rpc(w.ctx, w.ctx.vault.methods.initKeepers(keeper.publicKey, keeper.publicKey).accountsPartial({ authority: w.ctx.payer.publicKey, keepers: pda.keepers() }));
  const alice = fund(w.ctx);
  await fundHolder(w, alice);
  await rpc(w.ctx, mintIx(w, alice, 100n * 1_000_000_000n), [alice]);

  const burn = pda.burn();
  const burnA = await createAta(w.ctx, w.mintA, burn, TOKEN_PROGRAM_ID);
  const burnB = await createAta(w.ctx, w.mintB, burn, TOKEN_2022_PROGRAM_ID);
  const burnGravi = await createAta(w.ctx, w.graviMint, burn, TOKEN_PROGRAM_ID);
  // The keeper receives what it sells and holds GRAVI to deliver as the "market".
  const keeperA = await createAta(w.ctx, w.mintA, keeper.publicKey, TOKEN_PROGRAM_ID);
  const keeperGravi = await createAta(w.ctx, w.graviMint, keeper.publicKey, TOKEN_PROGRAM_ID);
  await mintTo(w.ctx, w.graviMint, keeperGravi, 1_000_000n * ONE_GRAVI);
  return { ...w, keeper, alice, burnA, burnB, burnGravi, keeperA, keeperGravi };
}
type BW = Awaited<ReturnType<typeof buybackWorld>>;

async function setPrice(w: BW, mint: PublicKey, price: bigint) {
  await rpc(w.ctx, w.ctx.vault.methods.setPrice(new BN(price.toString())).accountsPartial({ priceAuthority: w.keeper.publicKey, keepers: pda.keepers(), mint, assetPrice: pda.price(mint), pythPrice: null }), [w.keeper]);
}

const burnShares = (w: BW) => feeShares(w.shareMint).burnShares;

const redeemIx = (w: BW, shares: bigint, burnA = w.burnA) =>
  w.ctx.vault.methods
    .buybackRedeem(new BN(shares.toString()))
    .accountsPartial({ basket: w.basket, shareMint: w.shareMint, burnShares: burnShares(w), shareTokenProgram: TOKEN_PROGRAM_ID })
    .remainingAccounts(
      [
        [w.vaultA, burnA, w.mintA, TOKEN_PROGRAM_ID],
        [w.vaultB, w.burnB, w.mintB, TOKEN_2022_PROGRAM_ID],
      ].flatMap((row) => row.map((pubkey, i) => ({ pubkey, isSigner: false, isWritable: i < 2 }))),
    );

/** What a slice sells: A by default. */
interface Sold {
  mint: PublicKey;
  source: PublicKey;
  to: PublicKey;
}
const soldA = (w: BW): Sold => ({ mint: w.mintA, source: w.burnA, to: w.keeperA });

const startIx = (w: BW, amount: bigint, signer: Keypair = w.keeper, slippage = 50, refund = 0n, sold: Sold = soldA(w)) =>
  w.ctx.vault.methods
    .buybackStart(new BN(amount.toString()), slippage, new BN(refund.toString()))
    .accountsPartial({
      rebalancer: signer.publicKey,
      keepers: pda.keepers(),
      protocol: pda.protocol(),
      buyback: pda.buyback(),
      sellMint: sold.mint,
      sellSource: sold.source,
      rebalancerSell: sold.to,
      sellPrice: pda.price(sold.mint),
      graviPrice: pda.price(w.graviMint),
      burnGravi: w.burnGravi,
      sellTokenProgram: TOKEN_PROGRAM_ID,
      instructions: SYSVAR_INSTRUCTIONS_PUBKEY,
    })
    .instruction();

const finishIx = (w: BW, signer: Keypair = w.keeper) =>
  w.ctx.vault.methods
    .buybackFinish()
    .accountsPartial({
      rebalancer: signer.publicKey,
      keepers: pda.keepers(),
      protocol: pda.protocol(),
      buyback: pda.buyback(),
      graviMint: w.graviMint,
      burnGravi: w.burnGravi,
      graviTokenProgram: TOKEN_PROGRAM_ID,
    })
    .instruction();

const closeIx = (w: BW, mint: PublicKey, account: PublicKey, tokenProgram: PublicKey, mintAuthority: PublicKey | null, signer: Keypair = w.keeper) =>
  w.ctx.vault.methods
    .buybackClose()
    .accountsPartial({ rebalancer: signer.publicKey, keepers: pda.keepers(), protocol: pda.protocol(), mint, burnAccount: account, mintAuthority, tokenProgram })
    .instruction();

const lamports = (w: BW, key: PublicKey) => BigInt(w.ctx.svm.getAccount(key)?.lamports ?? 0);

/** The "market": the keeper delivers GRAVI to the burn PDA. */
const deliver = (w: BW, amount: bigint) =>
  createTransferCheckedInstruction(w.keeperGravi, w.graviMint, w.burnGravi, w.keeper.publicKey, amount, 9, [], TOKEN_PROGRAM_ID);

async function redeemAll(w: BW) {
  await rpc(w.ctx, redeemIx(w, balance(w.ctx, burnShares(w))));
  await setPrice(w, w.mintA, PRICE_A);
  await setPrice(w, w.mintB, PRICE_B);
  await setPrice(w, w.graviMint, PRICE_GRAVI);
}

describe("buyback and burn", () => {
  it("burn shares redeem into the burn PDA's own accounts, fee-free", async () => {
    const w = await buybackWorld();
    const held = balance(w.ctx, burnShares(w));
    assert.equal(held, 200_000_000n, "0.2 shares earmarked to burn");
    const [vA, vB, s] = [balance(w.ctx, w.vaultA), balance(w.ctx, w.vaultB), supply(w.ctx, w.shareMint)];
    await rpc(w.ctx, redeemIx(w, held));
    assert.equal(balance(w.ctx, burnShares(w)), 0n);
    assert.equal(supply(w.ctx, w.shareMint), s - held);
    // 0.2 shares x 2 A and x 0.5 B.
    assert.equal(balance(w.ctx, w.burnA), 400_000n);
    assert.equal(balance(w.ctx, w.burnB), 100_000_000n);
    assert.equal(balance(w.ctx, w.vaultA), vA - 400_000n);
    assert.equal(balance(w.ctx, w.vaultB), vB - 100_000_000n);
  });

  it("start and finish must use the burn PDA's own GRAVI account, no other it owns", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    // A second GRAVI account owned by the burn PDA, outside its canonical one.
    const other = Keypair.generate();
    await send(w.ctx, [
      SystemProgram.createAccount({ fromPubkey: w.ctx.payer.publicKey, newAccountPubkey: other.publicKey, lamports: 2_039_280, space: 165, programId: TOKEN_PROGRAM_ID }),
      createInitializeAccount3Instruction(other.publicKey, w.graviMint, pda.burn(), TOKEN_PROGRAM_ID),
    ], [other]);
    const alt = { ...w, burnGravi: other.publicKey };
    await expectError(send(w.ctx, [await startIx(alt, 400_000n), deliver(w, 40n * ONE_GRAVI), await finishIx(w)], [w.keeper]), "NotBasketVault");
    await expectError(send(w.ctx, [await startIx(w, 400_000n), deliver(w, 40n * ONE_GRAVI), await finishIx(alt)], [w.keeper]), "NotBasketVault");
  });

  it("redeeming refuses a destination the burn PDA doesn't own", async () => {
    const w = await buybackWorld();
    await expectError(rpc(w.ctx, redeemIx(w, 1_000_000n, w.keeperA)), "ConstituentMismatch");
  });

  it("a slice pays the keeper back up to $0.50 from the burn's tokens", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    // 0.35 A sold for 35 GRAVI, and 0.05 A ($0.05) for the keeper.
    await send(w.ctx, [await startIx(w, 350_000n, w.keeper, 50, 50_000n), deliver(w, 35n * ONE_GRAVI), await finishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.burnA), 0n);
    assert.equal(balance(w.ctx, w.keeperA), 400_000n);
    const w2 = await buybackWorld();
    await redeemAll(w2);
    // $0.50 is the most, checked before anything moves.
    await expectError(send(w2.ctx, [await startIx(w2, 1n, w2.keeper, 50, 500_001n), await finishIx(w2)], [w2.keeper]), "RefundTooLarge");
  });

  it("a slice swaps for GRAVI at the oracle price and burns all of it", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    // 0.4 A = $0.40 = 40 GRAVI at $0.01.
    const fair = 40n * ONE_GRAVI;
    const graviSupply = supply(w.ctx, w.graviMint);
    await send(w.ctx, [await startIx(w, 400_000n), deliver(w, fair), await finishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.burnA), 0n, "the slice left the burn PDA");
    assert.equal(balance(w.ctx, w.keeperA), 400_000n);
    assert.equal(balance(w.ctx, w.burnGravi), 0n, "nothing left unburned");
    assert.equal(supply(w.ctx, w.graviMint), graviSupply - fair);
    const b = await w.ctx.vault.account.buyback.fetch(pda.buyback());
    // Compared as numbers: on some Node versions a BN's toString() here came out "…NaN".
    assert.ok(b.totalBurned.eq(new BN(fair.toString())), `total burned ${b.totalBurned}`);
    assert.equal(b.slices, 1);
    assert.equal(b.pending, false);
  });

  it("a fill below the oracle floor reverts the slice", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    // 2% short of fair value against a 0.5% tolerance.
    await expectError(send(w.ctx, [await startIx(w, 400_000n), deliver(w, 39n * ONE_GRAVI + 200_000_000n), await finishIx(w)], [w.keeper]), "BelowOracleFloor");
    assert.equal(balance(w.ctx, w.burnA), 400_000n, "nothing left the burn PDA");
  });

  it("a start without a finish in the same transaction is refused", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    await expectError(send(w.ctx, [await startIx(w, 400_000n)], [w.keeper]), "BuybackFinishMissing");
  });

  it("only the configured rebalancer can run a buyback", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    const outsider = fund(w.ctx);
    await expectError(send(w.ctx, [await startIx(w, 400_000n, outsider), await finishIx(w, outsider)], [outsider]), "NotRebalancer");
  });

  it("tolerance is capped at 10%: fees, never a holder's deposit", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    // 9.5% short of fair value, accepted at a 10% tolerance.
    await send(w.ctx, [await startIx(w, 400_000n, w.keeper, 1_000), deliver(w, 36n * ONE_GRAVI + 200_000_000n), await finishIx(w)], [w.keeper]);
    await expectError(send(w.ctx, [await startIx(w, 1n, w.keeper, 1_001), await finishIx(w)], [w.keeper]), "BadParameter");
  });

  it("nothing of the program's may run between start and finish", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    const between = await w.ctx.vault.methods
      .setPrice(new BN(PRICE_A.toString()))
      .accountsPartial({ priceAuthority: w.keeper.publicKey, keepers: pda.keepers(), mint: w.mintA, assetPrice: pda.price(w.mintA), pythPrice: null })
      .instruction();
    await expectError(send(w.ctx, [await startIx(w, 400_000n), between, deliver(w, 40n * ONE_GRAVI), await finishIx(w)], [w.keeper]), "BuybackInterleaved");
  });

  it("stale prices are refused", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    const { warp } = await import("./helpers.ts");
    warp(w.ctx, 200);
    await expectError(send(w.ctx, [await startIx(w, 400_000n), deliver(w, 40n * ONE_GRAVI), await finishIx(w)], [w.keeper]), "BadPrice");
  });
});

describe("selling the burn's tokens for SOL, then all the SOL for GRAVI", () => {
  const PRICE_SOL = 100_000_000_000_000n; // $100
  const pdaSol = () => PublicKey.findProgramAddressSync([Buffer.from("buyback_sol")], VAULT_ID)[0];

  async function solWorld() {
    const w = await buybackWorld();
    // LiteSVM has no native mint account: SOL's, as on chain (9 decimals, no authority).
    const data = Buffer.alloc(MINT_SIZE);
    MintLayout.encode({ mintAuthorityOption: 0, mintAuthority: PublicKey.default, supply: 0n, decimals: 9, isInitialized: true, freezeAuthorityOption: 0, freezeAuthority: PublicKey.default }, data);
    w.ctx.svm.setAccount(NATIVE_MINT, { lamports: Number(w.ctx.svm.minimumBalanceForRentExemption(BigInt(MINT_SIZE))), data, owner: TOKEN_PROGRAM_ID, executable: false });
    const burnSol = await createAta(w.ctx, NATIVE_MINT, pda.burn(), TOKEN_PROGRAM_ID);
    // The keeper holds SOL to deliver as the "market", and receives SOL to sell for GRAVI.
    const keeperSol = await createAta(w.ctx, NATIVE_MINT, w.keeper.publicKey, TOKEN_PROGRAM_ID);
    await send(w.ctx, [SystemProgram.transfer({ fromPubkey: w.keeper.publicKey, toPubkey: keeperSol, lamports: 10_000_000_000 }), createSyncNativeInstruction(keeperSol)], [w.keeper]);
    await redeemAll(w);
    await setPrice(w, NATIVE_MINT, PRICE_SOL);
    return { ...w, burnSol, keeperSol };
  }
  type SW = Awaited<ReturnType<typeof solWorld>>;

  const solStartIx = (w: SW, amount: bigint, o: { signer?: Keypair; slippage?: number; mint?: PublicKey; source?: PublicKey; burnSol?: PublicKey } = {}) =>
    w.ctx.vault.methods
      .buybackSolStart(new BN(amount.toString()), o.slippage ?? 50, new BN(0))
      .accountsPartial({
        rebalancer: (o.signer ?? w.keeper).publicKey,
        keepers: pda.keepers(),
        protocol: pda.protocol(),
        buybackSol: pdaSol(),
        sellMint: o.mint ?? w.mintA,
        sellSource: o.source ?? w.burnA,
        rebalancerSell: w.keeperA,
        sellPrice: pda.price(o.mint ?? w.mintA),
        solPrice: pda.price(NATIVE_MINT),
        burnSol: o.burnSol ?? w.burnSol,
        sellTokenProgram: TOKEN_PROGRAM_ID,
        instructions: SYSVAR_INSTRUCTIONS_PUBKEY,
      })
      .instruction();
  const solFinishIx = (w: SW, o: { signer?: Keypair; burnSol?: PublicKey } = {}) =>
    w.ctx.vault.methods
      .buybackSolFinish()
      .accountsPartial({ rebalancer: (o.signer ?? w.keeper).publicKey, keepers: pda.keepers(), buybackSol: pdaSol(), burnSol: o.burnSol ?? w.burnSol })
      .instruction();
  /** The "market": the keeper delivers SOL to the burn PDA. */
  const deliverSol = (w: SW, lamports: bigint, to = w.burnSol) => createTransferCheckedInstruction(w.keeperSol, NATIVE_MINT, to, w.keeper.publicKey, lamports, 9, [], TOKEN_PROGRAM_ID);

  it("a token sells for SOL at the oracle price, and the SOL stays with the burn PDA", async () => {
    const w = await solWorld();
    // 0.4 A = $0.40 = 0.004 SOL at $100.
    await send(w.ctx, [await solStartIx(w, 400_000n), deliverSol(w, 4_000_000n), await solFinishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.burnA), 0n, "the token left the burn PDA");
    assert.equal(balance(w.ctx, w.burnSol), 4_000_000n, "its SOL is the burn PDA's");
    const b = await w.ctx.vault.account.buybackSol.fetch(pdaSol());
    assert.ok(b.totalSol.eq(new BN(4_000_000)), `total ${b.totalSol}`);
    assert.equal(b.slices, 1);
    assert.equal(b.pending, false);
  });

  it("then all the SOL buys GRAVI in one go, and it all burns", async () => {
    const w = await solWorld();
    await send(w.ctx, [await solStartIx(w, 400_000n), deliverSol(w, 4_000_000n), await solFinishIx(w)], [w.keeper]);
    const graviSupply = supply(w.ctx, w.graviMint);
    // 0.004 SOL = $0.40 = 40 GRAVI at $0.01.
    const sol: Sold = { mint: NATIVE_MINT, source: w.burnSol, to: w.keeperSol };
    await send(w.ctx, [await startIx(w, 4_000_000n, w.keeper, 50, 0n, sol), deliver(w, 40n * ONE_GRAVI), await finishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.burnSol), 0n);
    assert.equal(balance(w.ctx, w.burnGravi), 0n, "nothing left unburned");
    assert.equal(supply(w.ctx, w.graviMint), graviSupply - 40n * ONE_GRAVI);
  });

  it("a fill below the oracle floor reverts the sale; 10% is the most it allows", async () => {
    const w = await solWorld();
    // 12.5% short against 10%.
    await expectError(send(w.ctx, [await solStartIx(w, 400_000n, { slippage: 1_000 }), deliverSol(w, 3_500_000n), await solFinishIx(w)], [w.keeper]), "BelowOracleFloor");
    assert.equal(balance(w.ctx, w.burnA), 400_000n, "nothing left the burn PDA");
    await expectError(send(w.ctx, [await solStartIx(w, 400_000n, { slippage: 1_001 }), deliverSol(w, 4_000_000n), await solFinishIx(w)], [w.keeper]), "BadParameter");
    // 9.5% short, accepted at 10%.
    await send(w.ctx, [await solStartIx(w, 400_000n, { slippage: 1_000 }), deliverSol(w, 3_620_000n), await solFinishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.burnSol), 3_620_000n);
  });

  it("never SOL itself, nor GRAVI, which the burn PDA only burns", async () => {
    const w = await solWorld();
    await expectError(send(w.ctx, [await solStartIx(w, 1n, { mint: NATIVE_MINT, source: w.burnSol }), await solFinishIx(w)], [w.keeper]), "BadParameter");
    await mintTo(w.ctx, w.graviMint, w.burnGravi, ONE_GRAVI);
    await expectError(send(w.ctx, [await solStartIx(w, 1n, { mint: w.graviMint, source: w.burnGravi }), await solFinishIx(w)], [w.keeper]), "BadParameter");
  });

  it("the SOL must land in the burn PDA's own SOL account, no other it owns", async () => {
    const w = await solWorld();
    const other = Keypair.generate();
    await send(w.ctx, [
      SystemProgram.createAccount({ fromPubkey: w.ctx.payer.publicKey, newAccountPubkey: other.publicKey, lamports: 2_039_280, space: 165, programId: TOKEN_PROGRAM_ID }),
      createInitializeAccount3Instruction(other.publicKey, NATIVE_MINT, pda.burn(), TOKEN_PROGRAM_ID),
    ], [other]);
    await expectError(send(w.ctx, [await solStartIx(w, 400_000n, { burnSol: other.publicKey }), deliverSol(w, 4_000_000n, other.publicKey), await solFinishIx(w, { burnSol: other.publicKey })], [w.keeper]), "NotBasketVault");
  });

  it("a start needs its own finish later in the transaction, with nothing of the program's between", async () => {
    const w = await solWorld();
    await expectError(send(w.ctx, [await solStartIx(w, 400_000n), deliverSol(w, 4_000_000n)], [w.keeper]), "BuybackFinishMissing");
    // A GRAVI finish isn't a SOL sale's finish.
    await expectError(send(w.ctx, [await solStartIx(w, 400_000n), deliverSol(w, 4_000_000n), await finishIx(w)], [w.keeper]), "BuybackInterleaved");
  });

  it("only the configured rebalancer sells", async () => {
    const w = await solWorld();
    const outsider = fund(w.ctx);
    await expectError(send(w.ctx, [await solStartIx(w, 400_000n, { signer: outsider }), await solFinishIx(w, { signer: outsider })], [outsider]), "NotRebalancer");
  });
});

describe("closing the burn PDA's emptied accounts", () => {
  it("a sold-out burn account closes and its rent goes back to the rebalancer", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    await send(w.ctx, [await startIx(w, 400_000n), deliver(w, 40n * ONE_GRAVI), await finishIx(w)], [w.keeper]);
    const rent = lamports(w, w.burnA);
    const before = lamports(w, w.keeper.publicKey);
    await send(w.ctx, [await closeIx(w, w.mintA, w.burnA, TOKEN_PROGRAM_ID, w.ctx.payer.publicKey)], [w.keeper]);
    assert.equal(lamports(w, w.burnA), 0n, "closed");
    assert.equal(lamports(w, w.keeper.publicKey) - before, rent, "the rent is back with the rebalancer");
    // The next buyback opens it again, as the keeper's redeem does.
    await createAta(w.ctx, w.mintA, pda.burn(), TOKEN_PROGRAM_ID);
    assert.equal(balance(w.ctx, w.burnA), 0n);
  });

  it("an account still holding tokens isn't closed", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    await expectError(send(w.ctx, [await closeIx(w, w.mintB, w.burnB, TOKEN_2022_PROGRAM_ID, w.ctx.payer.publicKey)], [w.keeper]), "BurnAccountNotEmpty");
  });

  it("never the GRAVI account the buyback burns from", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    await send(w.ctx, [await startIx(w, 400_000n), deliver(w, 40n * ONE_GRAVI), await finishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.burnGravi), 0n);
    await expectError(send(w.ctx, [await closeIx(w, w.graviMint, w.burnGravi, TOKEN_PROGRAM_ID, w.ctx.payer.publicKey)], [w.keeper]), "BurnAccountKept");
  });

  it("never a share account, which every mint and redeem pays its burn fee into", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    assert.equal(balance(w.ctx, burnShares(w)), 0n);
    await expectError(send(w.ctx, [await closeIx(w, w.shareMint, burnShares(w), TOKEN_PROGRAM_ID, w.basket)], [w.keeper]), "BurnAccountKept");
    // Naming some other account as the share mint's authority doesn't get past it.
    await expectError(send(w.ctx, [await closeIx(w, w.shareMint, burnShares(w), TOKEN_PROGRAM_ID, w.ctx.payer.publicKey)], [w.keeper]), "BadParameter");
    await expectError(send(w.ctx, [await closeIx(w, w.shareMint, burnShares(w), TOKEN_PROGRAM_ID, null)], [w.keeper]), "BadParameter");
  });

  it("only the configured rebalancer closes them", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    await send(w.ctx, [await startIx(w, 400_000n), deliver(w, 40n * ONE_GRAVI), await finishIx(w)], [w.keeper]);
    const outsider = fund(w.ctx);
    await expectError(send(w.ctx, [await closeIx(w, w.mintA, w.burnA, TOKEN_PROGRAM_ID, w.ctx.payer.publicKey, outsider)], [outsider]), "NotRebalancer");
  });
});

describe("the GRAVI mint", () => {
  async function bareProtocol() {
    const ctx = setup();
    fakeProgramData(ctx, ctx.payer.publicKey);
    await rpc(
      ctx,
      ctx.vault.methods.initProtocol(PublicKey.default, 40, 5_000).accountsPartial({
        authority: ctx.payer.publicKey,
        protocol: pda.protocol(),
        programData: PublicKey.findProgramAddressSync([ctx.vault.programId.toBuffer()], new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111"))[0],
      }),
    );
    return ctx;
  }

  it("can be set once after launch, and never changed", async () => {
    const ctx = await bareProtocol();
    const gravi = PublicKey.unique();
    const set = (mint: PublicKey) => ctx.vault.methods.setGraviMint(mint).accountsPartial({ authority: ctx.payer.publicKey, protocol: pda.protocol() });
    await rpc(ctx, set(gravi));
    const p = await ctx.vault.account.protocol.fetch(pda.protocol());
    assert.ok(p.graviMint.equals(gravi));
    await expectError(rpc(ctx, set(PublicKey.unique())), "GraviMintAlreadySet");
  });

  it("only the protocol authority can set it", async () => {
    const ctx = await bareProtocol();
    const outsider = fund(ctx);
    await expectError(
      rpc(ctx, ctx.vault.methods.setGraviMint(PublicKey.unique()).accountsPartial({ authority: outsider.publicKey, protocol: pda.protocol() }), [outsider]),
      "NotProtocolAuthority",
    );
  });
});

void ata;
