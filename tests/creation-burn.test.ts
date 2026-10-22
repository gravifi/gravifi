// The GRAVI a creator burns per asset in a basket: set by the protocol authority within a hard
// ceiling, 0 = off, and a real burn (GRAVI's supply falls) from the creator's own account, paid
// by initialize_basket for each asset it starts with (seeded or not) and by add_constituent for
// each one added later.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  BN,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
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
import { listAsset, setCreationBurn, world, type World } from "./fixtures.ts";
import { Keypair, PublicKey } from "@solana/web3.js";

// The test GRAVI mint has 9 decimals.
const GRAVI = 1_000_000_000n;
const CEILING = 1_000_000n * GRAVI;

async function creationBurn(ctx: Ctx) {
  const c: any = await (ctx.vault.account as any).creationBurn.fetch(pda.creationBurn());
  return { perAsset: BigInt(c.perAsset.toString()), totalBurned: BigInt(c.totalBurned.toString()), baskets: c.baskets as number };
}

type Gravi = { mint: PublicKey; account: PublicKey; program?: PublicKey };

/** A fresh creator's basket over the world's two listed assets, naming the GRAVI accounts
 *  given (null leaves them out). */
function createIx(w: World, creator: Keypair, gravi: Gravi | null) {
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
    .accountsPartial({
      creator: creator.publicKey,
      basket,
      shareMint: pda.share(basket),
      shareTokenProgram: TOKEN_PROGRAM_ID,
      graviMint: gravi?.mint ?? null,
      creatorGravi: gravi?.account ?? null,
      graviTokenProgram: gravi ? (gravi.program ?? TOKEN_PROGRAM_ID) : null,
    })
    .remainingAccounts([
      { pubkey: w.mintA, isSigner: false, isWritable: false },
      { pubkey: pda.asset(w.mintA), isSigner: false, isWritable: false },
      { pubkey: w.mintB, isSigner: false, isWritable: false },
      { pubkey: pda.asset(w.mintB), isSigner: false, isWritable: false },
    ]);
}

/** A creator holding `amount` GRAVI. */
async function graviHolder(w: World, amount: bigint) {
  const creator = fund(w.ctx);
  const account = await createAta(w.ctx, w.graviMint, creator.publicKey);
  if (amount > 0n) await mintTo(w.ctx, w.graviMint, account, amount);
  return { creator, account };
}

describe("creation burn", () => {
  it("starts off: a basket is created without any GRAVI", async () => {
    const w = await world();
    assert.equal((await creationBurn(w.ctx)).perAsset, 0n);
    const creator = fund(w.ctx);
    await rpc(w.ctx, createIx(w, creator, null), [creator]);
  });

  it("only the protocol authority sets it", async () => {
    const w = await world();
    const stranger = fund(w.ctx);
    await expectError(setCreationBurn(w.ctx, w.graviMint, GRAVI, stranger), "NotProtocolAuthority");
  });

  it("can't go over 1,000,000 GRAVI per asset", async () => {
    const w = await world();
    await expectError(setCreationBurn(w.ctx, w.graviMint, CEILING + 1n), "CreationBurnTooHigh");
    await setCreationBurn(w.ctx, w.graviMint, CEILING);
    assert.equal((await creationBurn(w.ctx)).perAsset, CEILING);
  });

  it("is read against the protocol's GRAVI mint, not any mint", async () => {
    const w = await world();
    const other = await createMint(w.ctx, 0);
    await expectError(setCreationBurn(w.ctx, other, 1n), "GraviMintNotSet");
  });

  it("creating burns the amount once per asset from the creator, seeded or not, and GRAVI's supply falls by it", async () => {
    const w = await world();
    const perAsset = 100_000n * GRAVI;
    const amount = 2n * perAsset; // a two-asset basket
    await setCreationBurn(w.ctx, w.graviMint, perAsset);
    const { creator, account } = await graviHolder(w, amount + 5n);
    const supplyBefore = supply(w.ctx, w.graviMint);

    // Created, never seeded: the burn is already paid.
    await rpc(w.ctx, createIx(w, creator, { mint: w.graviMint, account }), [creator]);

    assert.equal(balance(w.ctx, account), 5n);
    assert.equal(supply(w.ctx, w.graviMint), supplyBefore - amount);
    const c = await creationBurn(w.ctx);
    assert.equal(c.totalBurned, amount);
    assert.equal(c.baskets, 1);
  });

  it("works with a Token-2022 GRAVI of 6 decimals, as on mainnet", async () => {
    const w = await world({ gravi: { program: TOKEN_2022_PROGRAM_ID, decimals: 6 } });
    const perAsset = 100_000n * 1_000_000n;
    const amount = 2n * perAsset;
    await expectError(setCreationBurn(w.ctx, w.graviMint, 1_000_001n * 1_000_000n), "CreationBurnTooHigh");
    await setCreationBurn(w.ctx, w.graviMint, perAsset);
    const creator = fund(w.ctx);
    const account = await createAta(w.ctx, w.graviMint, creator.publicKey, TOKEN_2022_PROGRAM_ID);
    await mintTo(w.ctx, w.graviMint, account, amount, TOKEN_2022_PROGRAM_ID);
    const supplyBefore = supply(w.ctx, w.graviMint);
    await rpc(w.ctx, createIx(w, creator, { mint: w.graviMint, account, program: TOKEN_2022_PROGRAM_ID }), [creator]);
    assert.equal(balance(w.ctx, account), 0n);
    assert.equal(supply(w.ctx, w.graviMint), supplyBefore - amount);
  });

  it("refuses a creation that leaves the GRAVI accounts out", async () => {
    const w = await world();
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    const creator = fund(w.ctx);
    await expectError(rpc(w.ctx, createIx(w, creator, null), [creator]), "CreationBurnAccountsMissing");
  });

  it("refuses a creator with too little GRAVI", async () => {
    const w = await world();
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    const { creator, account } = await graviHolder(w, 2n * GRAVI - 1n);
    await expectError(rpc(w.ctx, createIx(w, creator, { mint: w.graviMint, account }), [creator]), "insufficient funds");
    assert.equal(balance(w.ctx, account), 2n * GRAVI - 1n);
  });

  it("refuses a look-alike mint the creator made themselves", async () => {
    const w = await world();
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    const creator = fund(w.ctx);
    const fake = await createMint(w.ctx, 9);
    const account = await createAta(w.ctx, fake, creator.publicKey);
    await mintTo(w.ctx, fake, account, 2n * GRAVI);
    await expectError(rpc(w.ctx, createIx(w, creator, { mint: fake, account }), [creator]), "GraviMintNotSet");
  });

  it("can't burn someone else's GRAVI", async () => {
    const w = await world();
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    const { account: victims } = await graviHolder(w, 2n * GRAVI);
    const creator = fund(w.ctx);
    await expectError(rpc(w.ctx, createIx(w, creator, { mint: w.graviMint, account: victims }), [creator]), "owner does not match");
    assert.equal(balance(w.ctx, victims), 2n * GRAVI);
  });

  it("turned back to 0, it's off again", async () => {
    const w = await world();
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    await setCreationBurn(w.ctx, w.graviMint, 0n);
    const creator = fund(w.ctx);
    await rpc(w.ctx, createIx(w, creator, null), [creator]);
    assert.equal((await creationBurn(w.ctx)).baskets, 0);
  });
});

describe("burn-exempt wallet", () => {
  const exemptIx = (w: World, wallet: PublicKey, authority = w.ctx.payer) =>
    w.ctx.vault.methods.setBurnExempt(wallet).accountsPartial({ authority: authority.publicKey, protocol: pda.protocol() });

  it("only the protocol authority names it", async () => {
    const w = await world();
    const stranger = fund(w.ctx);
    await expectError(rpc(w.ctx, exemptIx(w, stranger.publicKey, stranger), [stranger]), "NotProtocolAuthority");
  });

  it("its baskets burn nothing, and aren't counted; everyone else's still burn", async () => {
    const w = await world();
    const perAsset = 100_000n * GRAVI;
    await setCreationBurn(w.ctx, w.graviMint, perAsset);
    const team = fund(w.ctx);
    await rpc(w.ctx, exemptIx(w, team.publicKey));
    // The creation burn's own fields are untouched.
    assert.equal((await creationBurn(w.ctx)).perAsset, perAsset);

    await rpc(w.ctx, createIx(w, team, null), [team]);
    assert.deepEqual(await creationBurn(w.ctx), { perAsset, totalBurned: 0n, baskets: 0 });

    const other = fund(w.ctx);
    await expectError(rpc(w.ctx, createIx(w, other, null), [other]), "CreationBurnAccountsMissing");
    const { creator, account } = await graviHolder(w, 2n * perAsset);
    await rpc(w.ctx, createIx(w, creator, { mint: w.graviMint, account }), [creator]);
    assert.equal((await creationBurn(w.ctx)).totalBurned, 2n * perAsset);
  });

  it("survives a change of the burn, and can be moved or removed", async () => {
    const w = await world();
    const team = fund(w.ctx);
    await rpc(w.ctx, exemptIx(w, team.publicKey));
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    // A protocol setting written after it leaves the wallet in place.
    await rpc(w.ctx, w.ctx.vault.methods.setProtocolFees(10, 5_000).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol() }));
    await rpc(w.ctx, createIx(w, team, null), [team]);

    const next = fund(w.ctx);
    await rpc(w.ctx, exemptIx(w, next.publicKey));
    await expectError(rpc(w.ctx, createIx(w, team, null), [team]), "CreationBurnAccountsMissing");
    await rpc(w.ctx, createIx(w, next, null), [next]);

    await rpc(w.ctx, exemptIx(w, PublicKey.default));
    await expectError(rpc(w.ctx, createIx(w, next, null), [next]), "CreationBurnAccountsMissing");
  });
});

describe("adding an asset", () => {
  /** A third listed asset, with the world basket's vault for it. */
  async function assetC(w: World) {
    const mint = await createMint(w.ctx, 6);
    await listAsset(w.ctx, mint);
    const vault = await createAta(w.ctx, mint, w.basket);
    return { mint, vault };
  }

  function addIx(w: World, c: { mint: PublicKey; vault: PublicKey }, gravi: Gravi | null) {
    return w.ctx.vault.methods.addConstituent().accountsPartial({
      authority: w.creator.publicKey,
      basket: w.basket,
      mint: c.mint,
      assetEntry: pda.asset(c.mint),
      vault: c.vault,
      graviMint: gravi?.mint ?? null,
      creatorGravi: gravi?.account ?? null,
      graviTokenProgram: gravi ? TOKEN_PROGRAM_ID : null,
    });
  }

  it("burns one asset's GRAVI, and doesn't count as a new basket", async () => {
    const w = await world();
    const perAsset = 100_000n * GRAVI;
    await setCreationBurn(w.ctx, w.graviMint, perAsset);
    const account = await createAta(w.ctx, w.graviMint, w.creator.publicKey);
    await mintTo(w.ctx, w.graviMint, account, perAsset);
    const supplyBefore = supply(w.ctx, w.graviMint);

    await rpc(w.ctx, addIx(w, await assetC(w), { mint: w.graviMint, account }), [w.creator]);

    assert.equal(balance(w.ctx, account), 0n);
    assert.equal(supply(w.ctx, w.graviMint), supplyBefore - perAsset);
    const c = await creationBurn(w.ctx);
    assert.equal(c.totalBurned, perAsset);
    assert.equal(c.baskets, 0);
  });

  it("the burn-exempt wallet adds assets without burning", async () => {
    const w = await world();
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    await rpc(
      w.ctx,
      w.ctx.vault.methods.setBurnExempt(w.creator.publicKey).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol() }),
    );
    await rpc(w.ctx, addIx(w, await assetC(w), null), [w.creator]);
    assert.equal((await creationBurn(w.ctx)).totalBurned, 0n);
  });

  it("refuses an added asset without its GRAVI while the burn is on", async () => {
    const w = await world();
    await setCreationBurn(w.ctx, w.graviMint, GRAVI);
    const c = await assetC(w);
    await expectError(rpc(w.ctx, addIx(w, c, null), [w.creator]), "CreationBurnAccountsMissing");
    const account = await createAta(w.ctx, w.graviMint, w.creator.publicKey);
    await mintTo(w.ctx, w.graviMint, account, GRAVI - 1n);
    await expectError(rpc(w.ctx, addIx(w, c, { mint: w.graviMint, account }), [w.creator]), "insufficient funds");
  });
});
