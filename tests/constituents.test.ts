// Changing what a creator-managed basket holds: an asset is added at 0% with no units (nothing
// moves), weighted with set_targets, and bought in by rebalancing; an asset set to 0% is sold
// all the way out by rebalancing, then removed.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { SYSVAR_INSTRUCTIONS_PUBKEY, type Keypair, type PublicKey } from "@solana/web3.js";
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
  rpc,
  send,
} from "./helpers.ts";
import { NO_GRAVI, feeShares, fundHolder, listAsset, mintIx, quads, redeemIx, world, type World } from "./fixtures.ts";

// World: 10 shares backed by 20 A ($1) and 5 B ($6): $20 + $30 = $50. C is a $1 token.
const PRICE = { A: 1_000_000_000_000n, B: 6_000_000_000_000n, C: 1_000_000_000_000n };
const ONE_A = 1_000_000n;
const ONE_C = 1_000_000n;

async function cWorld(opts: Parameters<typeof world>[0] = {}) {
  const w = await world(opts);
  const mintC = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
  await listAsset(w.ctx, mintC);
  const vaultC = await createAta(w.ctx, mintC, w.basket, TOKEN_PROGRAM_ID);
  const keeper = fund(w.ctx);
  await rpc(
    w.ctx,
    w.ctx.vault.methods.initKeepers(keeper.publicKey, keeper.publicKey).accountsPartial({ authority: w.ctx.payer.publicKey, keepers: pda.keepers() }),
  );
  // The keeper is the market: it holds A and C to deliver, and accounts to receive B and C.
  const keeperA = await createAta(w.ctx, w.mintA, keeper.publicKey, TOKEN_PROGRAM_ID);
  await mintTo(w.ctx, w.mintA, keeperA, 1_000n * ONE_A, TOKEN_PROGRAM_ID);
  const keeperB = await createAta(w.ctx, w.mintB, keeper.publicKey, TOKEN_2022_PROGRAM_ID);
  await mintTo(w.ctx, w.mintB, keeperB, 100n * 1_000_000_000n, TOKEN_2022_PROGRAM_ID);
  const keeperC = await createAta(w.ctx, mintC, keeper.publicKey, TOKEN_PROGRAM_ID);
  await mintTo(w.ctx, mintC, keeperC, 1_000n * ONE_C, TOKEN_PROGRAM_ID);
  return { ...w, mintC, vaultC, keeper, keeperA, keeperB, keeperC };
}
type CW = Awaited<ReturnType<typeof cWorld>>;

const addIx = (w: CW, signer: Keypair = w.creator, mint: PublicKey = w.mintC, vault: PublicKey = w.vaultC) =>
  w.ctx.vault.methods
    .addConstituent()
    .accountsPartial({ ...NO_GRAVI, authority: signer.publicKey, basket: w.basket, mint, assetEntry: pda.asset(mint), vault })
    .instruction();
const targetsIx = (w: CW, bps: number[]) =>
  w.ctx.vault.methods.setTargets(bps).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }).instruction();
const removeIx = (w: CW, index: number) =>
  w.ctx.vault.methods.removeConstituent(index).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }).instruction();

async function setPrices(w: CW, mints: PublicKey[]) {
  const by = new Map([[w.mintA.toBase58(), PRICE.A], [w.mintB.toBase58(), PRICE.B], [w.mintC.toBase58(), PRICE.C]]);
  for (const mint of mints) {
    await rpc(
      w.ctx,
      w.ctx.vault.methods
        .setPrice(new BN(by.get(mint.toBase58())!.toString()))
        .accountsPartial({ priceAuthority: w.keeper.publicKey, keepers: pda.keepers(), mint, assetPrice: pda.price(mint), pythPrice: null }),
      [w.keeper],
    );
  }
}

type Sleeve = { mint: PublicKey; vault: PublicKey; program: PublicKey; keeper: PublicKey; decimals: number };
const sleeves = (w: CW): Record<"A" | "B" | "C", Sleeve> => ({
  A: { mint: w.mintA, vault: w.vaultA, program: TOKEN_PROGRAM_ID, keeper: w.keeperA, decimals: 6 },
  B: { mint: w.mintB, vault: w.vaultB, program: TOKEN_2022_PROGRAM_ID, keeper: w.keeperB, decimals: 9 },
  C: { mint: w.mintC, vault: w.vaultC, program: TOKEN_PROGRAM_ID, keeper: w.keeperC, decimals: 6 },
});

/** One rebalance slice in one transaction: lend `amount` of the sold sleeve, deliver `get` of
 *  the bought one, finish. `held` lists the basket's sleeves in order. */
async function slice(w: CW, held: ("A" | "B" | "C")[], sell: "A" | "B" | "C", buy: "A" | "B" | "C", amount: bigint, get: bigint) {
  const s = sleeves(w);
  const book = held.flatMap((k) => [
    { pubkey: s[k].vault, isSigner: false, isWritable: false },
    { pubkey: pda.price(s[k].mint), isSigner: false, isWritable: false },
  ]);
  const start = await w.ctx.vault.methods
    .rebalanceStart(held.indexOf(sell), held.indexOf(buy), new BN(amount.toString()), 50, new BN(0))
    .accountsPartial({
      rebalancer: w.keeper.publicKey,
      keepers: pda.keepers(),
      basket: w.basket,
      rebalanceState: pda.rebalance(w.basket),
      sellMint: s[sell].mint,
      buyMint: s[buy].mint,
      sellVault: s[sell].vault,
      rebalancerSell: s[sell].keeper,
      sellTokenProgram: s[sell].program,
      instructions: SYSVAR_INSTRUCTIONS_PUBKEY,
    })
    .remainingAccounts(book)
    .instruction();
  const deliver = createTransferCheckedInstruction(s[buy].keeper, s[buy].mint, s[buy].vault, w.keeper.publicKey, get, s[buy].decimals, [], s[buy].program);
  const finish = await w.ctx.vault.methods
    .rebalanceFinish()
    .accountsPartial({ rebalancer: w.keeper.publicKey, keepers: pda.keepers(), basket: w.basket, shareMint: w.shareMint, rebalanceState: pda.rebalance(w.basket) })
    .remainingAccounts(book)
    .instruction();
  await send(w.ctx, [start, deliver, finish], [w.keeper]);
}

/** Mint/redeem accounts for a basket holding A, B and C. */
function quadsC(w: CW, user: PublicKey) {
  return [
    ...quads(w, user),
    { pubkey: w.vaultC, isSigner: false, isWritable: true },
    { pubkey: ata(w.mintC, user, TOKEN_PROGRAM_ID), isSigner: false, isWritable: true },
    { pubkey: w.mintC, isSigner: false, isWritable: false },
    { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
  ];
}

/** Mint and redeem on a basket holding A, B and C. */
const mintC = (w: CW, who: Keypair, shares: bigint) =>
  w.ctx.vault.methods
    .mintInKind(new BN(shares.toString()), new BN(shares.toString()))
    .accountsPartial({
      payer: who.publicKey,
      basket: w.basket,
      shareMint: w.shareMint,
      payerShares: ata(w.shareMint, who.publicKey, w.shareProgram),
      ...feeShares(w.shareMint, w.shareProgram),
      allowEntry: null,
      shareTokenProgram: w.shareProgram,
    })
    .remainingAccounts(quadsC(w, who.publicKey));
const redeemC = (w: CW, who: Keypair, shares: bigint) =>
  w.ctx.vault.methods
    .redeemInKind(new BN(shares.toString()), 0)
    .accountsPartial({
      payer: who.publicKey,
      basket: w.basket,
      shareMint: w.shareMint,
      payerShares: ata(w.shareMint, who.publicKey, w.shareProgram),
      ...feeShares(w.shareMint, w.shareProgram),
      shareTokenProgram: w.shareProgram,
    })
    .remainingAccounts(quadsC(w, who.publicKey));

async function holderC(w: CW) {
  const alice = fund(w.ctx);
  await fundHolder(w, alice);
  const c = await createAta(w.ctx, w.mintC, alice.publicKey, TOKEN_PROGRAM_ID);
  await mintTo(w.ctx, w.mintC, c, 1_000n * ONE_C, TOKEN_PROGRAM_ID);
  return alice;
}

const basketOf = (w: World) => w.ctx.vault.account.basket.fetch(w.basket);

describe("adding and removing assets", () => {
  it("a creator adds a listed asset at 0% and weights it in the same transaction", async () => {
    const w = await cWorld();
    await send(w.ctx, [await addIx(w), await targetsIx(w, [5_000, 3_000, 2_000])], [w.creator]);
    const b = await basketOf(w);
    assert.equal(b.constituentCount, 3);
    assert.equal(b.constituents[2].mint.toBase58(), w.mintC.toBase58());
    assert.equal(b.constituents[2].targetBps, 2_000);
    assert.equal(b.constituents[2].unitsPerShare.toString(), "0");
    // Nothing moved: units of the others are unchanged.
    assert.equal(b.constituents[0].unitsPerShare.toString(), "2000000");
  });

  it("holders mint and redeem while the new asset holds nothing", async () => {
    const w = await cWorld();
    await send(w.ctx, [await addIx(w), await targetsIx(w, [5_000, 3_000, 2_000])], [w.creator]);
    const alice = await holderC(w);
    const c0 = balance(w.ctx, ata(w.mintC, alice.publicKey));
    await rpc(w.ctx, mintC(w, alice, 1_000_000_000n), [alice]);
    await rpc(w.ctx, redeemC(w, alice, 1_000_000_000n), [alice]);
    assert.equal(balance(w.ctx, ata(w.mintC, alice.publicKey)), c0, "no C moved either way");
    assert.equal(balance(w.ctx, ata(w.shareMint, alice.publicKey)), 0n);
  });

  it("rebalancing buys the new asset in, and later mints deposit it", async () => {
    const w = await cWorld();
    await send(w.ctx, [await addIx(w), await targetsIx(w, [4_000, 4_000, 2_000])], [w.creator]);
    await setPrices(w, [w.mintA, w.mintB, w.mintC]);
    // B is 20 points over, C 20 under: one 5% slice, $2.50 of B for 2.5 C.
    await slice(w, ["A", "B", "C"], "B", "C", 416_666_666n, 2_500_000n);
    const b = await basketOf(w);
    assert.equal(balance(w.ctx, w.vaultC), 2_500_000n);
    assert.equal(b.constituents[2].unitsPerShare.toString(), "250000", "2.5 C over 10 shares");
    const alice = await holderC(w);
    const c0 = balance(w.ctx, ata(w.mintC, alice.publicKey));
    await rpc(w.ctx, mintC(w, alice, 1_000_000_000n), [alice]);
    assert.equal(c0 - balance(w.ctx, ata(w.mintC, alice.publicKey)), 250_000n, "a share now takes 0.25 C");
  });

  it("a new asset is bought in even when its weight is within the basket's drift tolerance", async () => {
    const w = await cWorld();
    // 40/60 held against 40/55/5 targets: C is 5 points under, exactly the 5% tolerance, and
    // the schedule isn't due. Being empty with a target is enough.
    await send(w.ctx, [await addIx(w), await targetsIx(w, [4_000, 5_500, 500])], [w.creator]);
    await setPrices(w, [w.mintA, w.mintB, w.mintC]);
    await slice(w, ["A", "B", "C"], "B", "C", 200_000_000n, 1_200_000n);
    assert.equal(balance(w.ctx, w.vaultC), 1_200_000n);
    // No longer empty, but the session the first slice opened carries on to target.
    await slice(w, ["A", "B", "C"], "B", "C", 200_000_000n, 1_200_000n);
    assert.equal(balance(w.ctx, w.vaultC), 2_400_000n);
  });

  it("an asset at 0% is sold out by rebalancing, then removed", async () => {
    const w = await cWorld();
    await send(w.ctx, [await addIx(w), await targetsIx(w, [4_000, 4_000, 2_000])], [w.creator]);
    await setPrices(w, [w.mintA, w.mintB, w.mintC]);
    await slice(w, ["A", "B", "C"], "B", "C", 416_666_666n, 2_500_000n);
    // Holding 2.5 C: it can't be removed yet, at any target.
    await expectError(send(w.ctx, [await removeIx(w, 2)], [w.creator]), "ConstituentNotEmpty");
    await send(w.ctx, [await targetsIx(w, [6_000, 4_000, 0])], [w.creator]);
    await expectError(send(w.ctx, [await removeIx(w, 2)], [w.creator]), "ConstituentNotEmpty");

    // Most of C goes in one slice, leaving 0.01 C: 2 bps of the basket, under the 10 bp
    // trading threshold. With A put on target the basket isn't due, but an emptying slice
    // may still sell the rest.
    await slice(w, ["A", "B", "C"], "C", "A", 2_490_000n, 2_490_000n);
    assert.equal(balance(w.ctx, w.vaultC), 10_000n);
    await send(w.ctx, [await targetsIx(w, [4_500, 5_500, 0])], [w.creator]);
    await slice(w, ["A", "B", "C"], "C", "A", 10_000n, 10_000n);
    assert.equal(balance(w.ctx, w.vaultC), 0n);
    assert.equal((await basketOf(w)).constituents[2].unitsPerShare.toString(), "0");

    await send(w.ctx, [await removeIx(w, 2)], [w.creator]);
    const b = await basketOf(w);
    assert.equal(b.constituentCount, 2);
    assert.deepEqual(b.constituents.map((c) => c.mint.toBase58()), [w.mintA.toBase58(), w.mintB.toBase58()]);
    // Back to a two-asset basket: mint and redeem take the two usual sleeves.
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await rpc(w.ctx, mintIx(w as unknown as World, alice, 1_000_000_000n), [alice]);
    await rpc(w.ctx, redeemIx(w as unknown as World, alice, 1_000_000_000n), [alice]);
  });

  it("a 0% sleeve can be emptied when the basket isn't due, and nothing else can trade", async () => {
    const w = await cWorld();
    // Targets set to what the basket holds (40% A, 60% B): nothing is due, no session is open.
    await send(w.ctx, [await addIx(w), await targetsIx(w, [4_000, 6_000, 0])], [w.creator]);
    await setPrices(w, [w.mintA, w.mintB, w.mintC]);
    // 0.01 C lands in its vault (a donation, or what a sell-out left): 2 bps of the basket.
    await send(w.ctx, [createTransferCheckedInstruction(w.keeperC, w.mintC, w.vaultC, w.keeper.publicKey, 10_000n, 6, [], TOKEN_PROGRAM_ID)], [w.keeper]);
    await expectError(slice(w, ["A", "B", "C"], "B", "A", 1_000_000n, 6_000_000n), "RebalanceNotDue");
    await slice(w, ["A", "B", "C"], "C", "A", 10_000n, 10_000n);
    assert.equal(balance(w.ctx, w.vaultC), 0n);
    await send(w.ctx, [await removeIx(w, 2)], [w.creator]);
    assert.equal((await basketOf(w)).constituentCount, 2);
  });

  it("a 0% sleeve trades without waiting for the basket's schedule, into any sleeve under target", async () => {
    const w = await cWorld();
    await send(w.ctx, [await addIx(w), await targetsIx(w, [4_000, 6_000, 0])], [w.creator]);
    await setPrices(w, [w.mintA, w.mintB, w.mintC]);
    // 0.40 C: ~79 bps of the basket, above the one-slice tail, so it goes in normal slices.
    await send(w.ctx, [createTransferCheckedInstruction(w.keeperC, w.mintC, w.vaultC, w.keeper.publicKey, 400_000n, 6, [], TOKEN_PROGRAM_ID)], [w.keeper]);
    // Into B (~48 bps under), at most B's gap: $0.24 of C.
    await expectError(slice(w, ["A", "B", "C"], "C", "B", 400_000n, 66_666_666n), "SliceTooLarge");
    await slice(w, ["A", "B", "C"], "C", "B", 240_000n, 40_000_000n);
    assert.equal(balance(w.ctx, w.vaultC), 160_000n);
    // What's left (~32 bps) is the tail: it goes in one slice.
    await slice(w, ["A", "B", "C"], "C", "A", 160_000n, 160_000n);
    assert.equal(balance(w.ctx, w.vaultC), 0n);
  });

  it("only the creator of a creator-run basket can add or remove", async () => {
    const w = await cWorld();
    const outsider = fund(w.ctx);
    await expectError(send(w.ctx, [await addIx(w, outsider)], [outsider]), "NotMandateAuthority");
    const im = await cWorld({ control: "immutable" });
    await expectError(send(im.ctx, [await addIx(im)], [im.creator]), "BasketImmutable");
  });

  it("an unlisted or duplicate asset, or a missing vault, is refused", async () => {
    const w = await cWorld();
    const unlisted = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
    const unlistedVault = await createAta(w.ctx, unlisted, w.basket, TOKEN_PROGRAM_ID);
    await expectError(send(w.ctx, [await addIx(w, w.creator, unlisted, unlistedVault)], [w.creator]), "AssetNotListed");
    await expectError(send(w.ctx, [await addIx(w, w.creator, w.mintA, w.vaultA)], [w.creator]), "DuplicateConstituent");
    const listed = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
    await listAsset(w.ctx, listed);
    await assert.rejects(send(w.ctx, [await addIx(w, w.creator, listed, ata(listed, w.basket))], [w.creator]), "a vault that doesn't exist");
  });

  it("a basket holds at most 14 assets and keeps at least 2", async () => {
    const w = await cWorld();
    for (let i = 0; i < 12; i++) {
      const m = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
      await listAsset(w.ctx, m);
      const v = await createAta(w.ctx, m, w.basket, TOKEN_PROGRAM_ID);
      await send(w.ctx, [await addIx(w, w.creator, m, v)], [w.creator]);
    }
    assert.equal((await basketOf(w)).constituentCount, 14);
    await expectError(send(w.ctx, [await addIx(w)], [w.creator]), "TooManyConstituents");
    // Empty sleeves at 0% come out, down to two.
    for (let i = 13; i >= 2; i--) await send(w.ctx, [await removeIx(w, i)], [w.creator]);
    await send(w.ctx, [await targetsIx(w, [10_000, 0])], [w.creator]);
    await expectError(send(w.ctx, [await removeIx(w, 1)], [w.creator]), "TooFewConstituents");
  });
});
