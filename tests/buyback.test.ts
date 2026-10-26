// The GRAVI buyback-and-burn: burn-earmarked shares are redeemed into the burn PDA's own
// accounts, lent to the rebalancer a slice at a time to swap for GRAVI in the same
// transaction, and every GRAVI that comes back is burned once it clears the oracle floor.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { Keypair, PublicKey, SYSVAR_INSTRUCTIONS_PUBKEY, SystemProgram } from "@solana/web3.js";
import { createInitializeAccount3Instruction, createTransferCheckedInstruction } from "@solana/spl-token";
import {
  BN,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
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

const startIx = (w: BW, amount: bigint, signer: Keypair = w.keeper, slippage = 50, refund = 0n) =>
  w.ctx.vault.methods
    .buybackStart(new BN(amount.toString()), slippage, new BN(refund.toString()))
    .accountsPartial({
      rebalancer: signer.publicKey,
      keepers: pda.keepers(),
      protocol: pda.protocol(),
      buyback: pda.buyback(),
      sellMint: w.mintA,
      sellSource: w.burnA,
      rebalancerSell: w.keeperA,
      sellPrice: pda.price(w.mintA),
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

  it("a slice pays the keeper back up to $0.05 from the burn's tokens", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    // 0.35 A sold for 35 GRAVI, and 0.05 A ($0.05) for the keeper.
    await send(w.ctx, [await startIx(w, 350_000n, w.keeper, 50, 50_000n), deliver(w, 35n * ONE_GRAVI), await finishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.burnA), 0n);
    assert.equal(balance(w.ctx, w.keeperA), 400_000n);
    const w2 = await buybackWorld();
    await redeemAll(w2);
    await expectError(send(w2.ctx, [await startIx(w2, 300_000n, w2.keeper, 50, 50_001n), deliver(w2, 30n * ONE_GRAVI), await finishIx(w2)], [w2.keeper]), "RefundTooLarge");
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

  it("tolerance is capped at 3%, wider than a rebalance's 1.5%", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    // 2.5% short of fair value, accepted at a 3% tolerance.
    await send(w.ctx, [await startIx(w, 400_000n, w.keeper, 300), deliver(w, 39n * ONE_GRAVI), await finishIx(w)], [w.keeper]);
    await expectError(send(w.ctx, [await startIx(w, 1n, w.keeper, 301), await finishIx(w)], [w.keeper]), "BadParameter");
  });

  it("stale prices are refused", async () => {
    const w = await buybackWorld();
    await redeemAll(w);
    const { warp } = await import("./helpers.ts");
    warp(w.ctx, 200);
    await expectError(send(w.ctx, [await startIx(w, 400_000n), deliver(w, 40n * ONE_GRAVI), await finishIx(w)], [w.keeper]), "BadPrice");
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
