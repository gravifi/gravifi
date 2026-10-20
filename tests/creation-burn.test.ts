// The GRAVI a creator burns to create a basket: set by the protocol authority within a hard
// ceiling, 0 = off, and a real burn (GRAVI's supply falls) from the creator's own account. It's
// paid by the seed, not by initialize_basket, so a basket that never goes live burns nothing.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
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
  supply,
  type Ctx,
} from "./helpers.ts";
import { SEED_SHARES, UNITS_A, UNITS_B, feeShares, quads, setCreationBurn, world, type World } from "./fixtures.ts";
import { Keypair, PublicKey } from "@solana/web3.js";

// The test GRAVI mint has 9 decimals.
const GRAVI = 1_000_000_000n;
const CEILING = 10_000_000n * GRAVI;

type Gravi = { mint: PublicKey; account: PublicKey; program?: PublicKey };

async function creationBurn(ctx: Ctx) {
  const c: any = await (ctx.vault.account as any).creationBurn.fetch(pda.creationBurn());
  return { amount: BigInt(c.amount.toString()), totalBurned: BigInt(c.totalBurned.toString()), baskets: c.baskets as number };
}

async function seeded(w: World): Promise<boolean> {
  return ((await (w.ctx.vault.account as any).basket.fetch(w.basket)) as any).seeded;
}

/** A fresh creator's basket over the world's two listed assets. */
function createIx(w: World, creator: Keypair) {
  const basketId = Keypair.generate().publicKey.toBytes().slice(0, 16);
  const basket = pda.basket(creator.publicKey, basketId);
  return w.ctx.vault.methods
    .initializeBasket({
      basketId: Array.from(basketId),
      access: { public: {} },
      control: { creator: {} },
      rebalanceMode: { hybrid: {} },
      intervalSecs: new BN(86_400),
      driftBps: 500,
      mgmtFeeBps: 20,
      mints: [w.mintA, w.mintB],
      targetBps: [5_000, 5_000],
    })
    .accountsPartial({ creator: creator.publicKey, basket, shareMint: pda.share(basket), shareTokenProgram: TOKEN_PROGRAM_ID })
    .remainingAccounts([
      { pubkey: w.mintA, isSigner: false, isWritable: false },
      { pubkey: pda.asset(w.mintA), isSigner: false, isWritable: false },
      { pubkey: w.mintB, isSigner: false, isWritable: false },
      { pubkey: pda.asset(w.mintB), isSigner: false, isWritable: false },
    ]);
}

/** The world's (unseeded) basket seeded by its creator, naming the GRAVI accounts given (null
 *  leaves them out). */
function seedIx(w: World, gravi: Gravi | null) {
  return w.ctx.vault.methods
    .seedBasket(new BN(SEED_SHARES.toString()), [new BN(UNITS_A.toString()), new BN(UNITS_B.toString())])
    .accountsPartial({
      payer: w.creator.publicKey,
      basket: w.basket,
      shareMint: w.shareMint,
      payerShares: ata(w.shareMint, w.creator.publicKey, w.shareProgram),
      ...feeShares(w.shareMint, w.shareProgram),
      shareTokenProgram: w.shareProgram,
      graviMint: gravi?.mint ?? null,
      creatorGravi: gravi?.account ?? null,
      graviTokenProgram: gravi ? (gravi.program ?? TOKEN_PROGRAM_ID) : null,
    })
    .remainingAccounts(quads(w, w.creator.publicKey));
}

/** Gives `who` a GRAVI account holding `amount`. */
async function giveGravi(w: World, who: PublicKey, amount: bigint, program = TOKEN_PROGRAM_ID) {
  const account = await createAta(w.ctx, w.graviMint, who, program);
  if (amount > 0n) await mintTo(w.ctx, w.graviMint, account, amount, program);
  return account;
}

describe("creation burn", () => {
  it("starts off: a basket is created and seeded without any GRAVI", async () => {
    const w = await world({ seed: false });
    assert.equal((await creationBurn(w.ctx)).amount, 0n);
    await rpc(w.ctx, seedIx(w, null), [w.creator]);
    assert.equal(await seeded(w), true);
  });

  it("only the protocol authority sets it", async () => {
    const w = await world();
    const stranger = fund(w.ctx);
    await expectError(setCreationBurn(w.ctx, w.graviMint, GRAVI, stranger), "NotProtocolAuthority");
  });

  it("can't go over 10,000,000 GRAVI", async () => {
    const w = await world();
    await expectError(setCreationBurn(w.ctx, w.graviMint, CEILING + 1n), "CreationBurnTooHigh");
    await setCreationBurn(w.ctx, w.graviMint, CEILING);
    assert.equal((await creationBurn(w.ctx)).amount, CEILING);
  });

  it("is read against the protocol's GRAVI mint, not any mint", async () => {
    const w = await world();
    const other = await createMint(w.ctx, 0);
    await expectError(setCreationBurn(w.ctx, other, 1n), "GraviMintNotSet");
  });

  it("creating a basket burns nothing: the burn goes with its seed", async () => {
    const w = await world();
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    const creator = fund(w.ctx);
    const account = await giveGravi(w, creator.publicKey, GRAVI);
    await rpc(w.ctx, createIx(w, creator), [creator]);
    assert.equal(balance(w.ctx, account), GRAVI);
    assert.equal((await creationBurn(w.ctx)).baskets, 0);
  });

  it("the seed burns exactly the amount from the creator, and GRAVI's supply falls by it", async () => {
    const w = await world({ seed: false });
    const amount = 1_000_000n * GRAVI;
    await setCreationBurn(w.ctx, w.graviMint, amount);
    const account = await giveGravi(w, w.creator.publicKey, amount + 5n);
    const supplyBefore = supply(w.ctx, w.graviMint);

    await rpc(w.ctx, seedIx(w, { mint: w.graviMint, account }), [w.creator]);

    assert.equal(await seeded(w), true);
    assert.equal(balance(w.ctx, account), 5n);
    assert.equal(supply(w.ctx, w.graviMint), supplyBefore - amount);
    const c = await creationBurn(w.ctx);
    assert.equal(c.totalBurned, amount);
    assert.equal(c.baskets, 1);
  });

  it("works with a Token-2022 GRAVI of 6 decimals, as on mainnet", async () => {
    const w = await world({ seed: false, gravi: { program: TOKEN_2022_PROGRAM_ID, decimals: 6 } });
    const amount = 1_000_000n * 1_000_000n;
    await expectError(setCreationBurn(w.ctx, w.graviMint, 10_000_001n * 1_000_000n), "CreationBurnTooHigh");
    await setCreationBurn(w.ctx, w.graviMint, amount);
    const account = await giveGravi(w, w.creator.publicKey, amount, TOKEN_2022_PROGRAM_ID);
    const supplyBefore = supply(w.ctx, w.graviMint);
    await rpc(w.ctx, seedIx(w, { mint: w.graviMint, account, program: TOKEN_2022_PROGRAM_ID }), [w.creator]);
    assert.equal(balance(w.ctx, account), 0n);
    assert.equal(supply(w.ctx, w.graviMint), supplyBefore - amount);
  });

  it("a seed that fails burns nothing, and the basket can still be seeded", async () => {
    const w = await world({ seed: false });
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    const account = await giveGravi(w, w.creator.publicKey, GRAVI - 1n);
    await expectError(rpc(w.ctx, seedIx(w, { mint: w.graviMint, account }), [w.creator]), "insufficient funds");
    assert.equal(balance(w.ctx, account), GRAVI - 1n);
    assert.equal(await seeded(w), false);
    assert.equal((await creationBurn(w.ctx)).totalBurned, 0n);

    await mintTo(w.ctx, w.graviMint, account, 1n);
    await rpc(w.ctx, seedIx(w, { mint: w.graviMint, account }), [w.creator]);
    assert.equal(balance(w.ctx, account), 0n);
    assert.equal(await seeded(w), true);
  });

  it("refuses a seed that leaves the GRAVI accounts out", async () => {
    const w = await world({ seed: false });
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    await expectError(rpc(w.ctx, seedIx(w, null), [w.creator]), "CreationBurnAccountsMissing");
    assert.equal(await seeded(w), false);
  });

  it("refuses a look-alike mint the creator made themselves", async () => {
    const w = await world({ seed: false });
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    const fake = await createMint(w.ctx, 9);
    const account = await createAta(w.ctx, fake, w.creator.publicKey);
    await mintTo(w.ctx, fake, account, GRAVI);
    await expectError(rpc(w.ctx, seedIx(w, { mint: fake, account }), [w.creator]), "GraviMintNotSet");
  });

  it("can't burn someone else's GRAVI", async () => {
    const w = await world({ seed: false });
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    const victims = await giveGravi(w, fund(w.ctx).publicKey, GRAVI);
    await expectError(rpc(w.ctx, seedIx(w, { mint: w.graviMint, account: victims }), [w.creator]), "owner does not match");
    assert.equal(balance(w.ctx, victims), GRAVI);
  });

  it("turned back to 0, it's off again", async () => {
    const w = await world({ seed: false });
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    await setCreationBurn(w.ctx, w.graviMint, 0n);
    await rpc(w.ctx, seedIx(w, null), [w.creator]);
    assert.equal((await creationBurn(w.ctx)).baskets, 0);
  });
});
