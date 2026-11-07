// Bounded prices: an asset with a Pyth feed is held within Pyth's confidence of a fresh, fully
// verified Pyth price; any other within 20% of a reference that rolls forward hourly, which an
// asset's first price only seeds. Pyth's accounts are written into LiteSVM byte for byte.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { Keypair, PublicKey, SystemProgram, TransactionInstruction } from "@solana/web3.js";
import { createInitializeMint2Instruction } from "@solana/spl-token";
import { BN, TOKEN_2022_PROGRAM_ID, createMint, expectError, fund, now, pda, rpc, send, warp, type Ctx } from "./helpers.ts";
import { world, initKeepers } from "./fixtures.ts";

const RECEIVER = new PublicKey("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");
const DISCRIMINATOR = Buffer.from([34, 241, 35, 99, 157, 126, 244, 205]);
const FEED = Buffer.alloc(32, 7);
const E12 = 1_000_000_000_000n;
const $ = (usd: number) => BigInt(Math.round(usd * 1e6)) * 1_000_000n; // USD x1e12

async function priceWorld() {
  const w = await world();
  const keeper = fund(w.ctx);
  await initKeepers(w.ctx, keeper.publicKey, keeper.publicKey);
  return { ...w, keeper };
}
type PW = Awaited<ReturnType<typeof priceWorld>>;

const priceIx = (w: PW, mint: PublicKey, price: bigint, pyth: PublicKey | null = null) =>
  w.ctx.vault.methods
    .setPrice(new BN(price.toString()))
    .accountsPartial({ priceAuthority: w.keeper.publicKey, keepers: pda.keepers(), mint, assetPrice: pda.price(mint), pythPrice: pyth });
const post = (w: PW, mint: PublicKey, price: bigint, pyth: PublicKey | null = null) => rpc(w.ctx, priceIx(w, mint, price, pyth), [w.keeper]);
const posted = async (w: PW, mint: PublicKey) => BigInt((await w.ctx.vault.account.assetPrice.fetch(pda.price(mint))).priceE12.toString());
const reference = async (w: PW, mint: PublicKey) => BigInt((await w.ctx.vault.account.priceGuard.fetch(pda.guard(mint))).referenceE12.toString());
/** A first price without Pyth: seeded, then confirmed a quarter of an hour later. */
async function seed(w: PW, mint: PublicKey, price: bigint) {
  await post(w, mint, price);
  warp(w.ctx, 900);
  await post(w, mint, price);
}

const feedIx = (w: PW, mint: PublicKey, feed: Buffer, authority = w.ctx.payer) =>
  w.ctx.vault.methods.setPriceFeed(Array.from(feed)).accountsPartial({ authority: authority.publicKey, protocol: pda.protocol(), mint, priceGuard: pda.guard(mint) });

/** A `PriceUpdateV2` account as Pyth's receiver writes it. */
function pythAccount(ctx: Ctx, o: { price: bigint; conf?: bigint; expo?: number; age?: number; feed?: Buffer; full?: boolean; owner?: PublicKey }) {
  const address = Keypair.generate().publicKey;
  const level = o.full === false ? Buffer.from([0, 5]) : Buffer.from([1]);
  const n = (v: bigint, size: 8 | 4, signed = true) => {
    const b = Buffer.alloc(size);
    if (size === 4) b.writeInt32LE(Number(v));
    else if (signed) b.writeBigInt64LE(v);
    else b.writeBigUInt64LE(v);
    return b;
  };
  const publish = BigInt(now(ctx) - (o.age ?? 5));
  const data = Buffer.concat([
    DISCRIMINATOR,
    Buffer.alloc(32), // write authority
    level,
    o.feed ?? FEED,
    n(o.price, 8),
    n(o.conf ?? 0n, 8, false),
    n(BigInt(o.expo ?? -8), 4),
    n(publish, 8),
    n(publish - 1n, 8),
    n(o.price, 8),
    n(o.conf ?? 0n, 8, false),
    n(1n, 8, false), // posted slot
  ]);
  ctx.svm.setAccount(address, { lamports: 10_000_000, data, owner: o.owner ?? RECEIVER, executable: false });
  return address;
}

describe("price band", () => {
  it("a new asset's first price only seeds the reference: nothing trades at it until a post 15 minutes later agrees", async () => {
    const w = await priceWorld();
    await post(w, w.mintA, $(10));
    assert.equal(await reference(w, w.mintA), $(10));
    // No usable price: every reader (slices, stops, the buyback) wants one above zero and fresh.
    const unusable = await w.ctx.vault.account.assetPrice.fetch(pda.price(w.mintA));
    assert.equal(BigInt(unusable.priceE12.toString()), 0n);
    assert.equal(unusable.updatedAt.toNumber(), 0);
    // Agreeing too early changes nothing.
    warp(w.ctx, 899);
    await post(w, w.mintA, $(11));
    assert.equal(await posted(w, w.mintA), 0n);
    // A quarter of the reference window after the seed, a post within 20% of it is the price.
    warp(w.ctx, 1);
    await post(w, w.mintA, $(11));
    assert.equal(await posted(w, w.mintA), $(11));
    assert.equal(await reference(w, w.mintA), $(10));
  });

  it("a post that disagrees with an unconfirmed seed replaces it, and the quarter hour starts again", async () => {
    const w = await priceWorld();
    await post(w, w.mintA, $(1_000)); // a mistyped first price
    warp(w.ctx, 900);
    await post(w, w.mintA, $(10)); // 99% off: the new seed, nothing usable yet
    assert.equal(await posted(w, w.mintA), 0n);
    assert.equal(await reference(w, w.mintA), $(10));
    await post(w, w.mintA, $(10));
    assert.equal(await posted(w, w.mintA), 0n);
    warp(w.ctx, 900);
    await post(w, w.mintA, $(10));
    assert.equal(await posted(w, w.mintA), $(10));
    // Once there's a price, the band holds as before.
    await expectError(post(w, w.mintA, $(1_000)), "PriceMoveTooLarge");
  });

  it("an asset that already has a price keeps working as before: no new quarter hour", async () => {
    const w = await priceWorld();
    await seed(w, w.mintA, $(10));
    await post(w, w.mintA, $(11));
    assert.equal(await posted(w, w.mintA), $(11));
  });

  it("a price may move up to 20% from the reference, and no further", async () => {
    const w = await priceWorld();
    await seed(w, w.mintA, $(10));
    await post(w, w.mintA, $(12));
    await post(w, w.mintA, $(8));
    await expectError(post(w, w.mintA, $(12.01)), "PriceMoveTooLarge");
    await expectError(post(w, w.mintA, $(7.99)), "PriceMoveTooLarge");
    assert.equal(await posted(w, w.mintA), $(8));
  });

  it("within the hour, steps don't compound: the reference holds until an hour has passed", async () => {
    const w = await priceWorld();
    await post(w, w.mintA, $(10));
    warp(w.ctx, 1_800);
    await post(w, w.mintA, $(12));
    await expectError(post(w, w.mintA, $(14)), "PriceMoveTooLarge");
    warp(w.ctx, 1_801); // an hour since the reference: it rolls to the next accepted price
    await post(w, w.mintA, $(12));
    await post(w, w.mintA, $(14.4));
    assert.equal(await posted(w, w.mintA), $(14.4));
  });

  it("a guard that starts after prices were posted is held to the last one", async () => {
    const w = await priceWorld();
    await seed(w, w.mintA, $(10));
    // As on a cluster upgraded with prices already posted: the price stays, the guard is new.
    w.ctx.svm.setAccount(pda.guard(w.mintA), { lamports: 0, data: Buffer.alloc(0), owner: SystemProgram.programId, executable: false });
    await expectError(post(w, w.mintA, $(20)), "PriceMoveTooLarge");
    await post(w, w.mintA, $(11));
  });

  it("only the keepers' price authority posts prices", async () => {
    const w = await priceWorld();
    const stranger = fund(w.ctx);
    const ix = w.ctx.vault.methods
      .setPrice(new BN($(10).toString()))
      .accountsPartial({ priceAuthority: stranger.publicKey, keepers: pda.keepers(), mint: w.mintA, assetPrice: pda.price(w.mintA), pythPrice: null });
    await expectError(rpc(w.ctx, ix, [stranger]), "NotPriceAuthority");
  });
});

describe("Pyth check", () => {
  /** Mint A with a feed, and a first price at $10. */
  async function fed() {
    const w = await priceWorld();
    await rpc(w.ctx, feedIx(w, w.mintA, FEED));
    await post(w, w.mintA, $(10), pythAccount(w.ctx, { price: 1_000_000_000n }));
    return w;
  }

  it("only the protocol authority names a feed", async () => {
    const w = await priceWorld();
    const stranger = fund(w.ctx);
    await expectError(rpc(w.ctx, feedIx(w, w.mintA, FEED, stranger), [stranger]), "NotProtocolAuthority");
  });

  it("with a feed, a price needs a fresh, fully verified Pyth price for it", async () => {
    const w = await fed();
    await expectError(post(w, w.mintA, $(10)), "PythPriceMissing");
    await expectError(post(w, w.mintA, $(10), pythAccount(w.ctx, { price: 1_000_000_000n, owner: Keypair.generate().publicKey })), "PythPriceInvalid");
    await expectError(post(w, w.mintA, $(10), pythAccount(w.ctx, { price: 1_000_000_000n, feed: Buffer.alloc(32, 9) })), "PythPriceInvalid");
    await expectError(post(w, w.mintA, $(10), pythAccount(w.ctx, { price: 1_000_000_000n, full: false })), "PythPriceInvalid");
    await expectError(post(w, w.mintA, $(10), pythAccount(w.ctx, { price: 1_000_000_000n, age: 301 })), "PythPriceStale");
    await post(w, w.mintA, $(10), pythAccount(w.ctx, { price: 1_000_000_000n, age: 299 }));
  });

  it("a Pyth price published more than 10 seconds ahead of the cluster's clock is refused", async () => {
    const w = await fed();
    warp(w.ctx, 1_000);
    // Ahead, the age check alone would let it stand for as long as it's ahead.
    await expectError(post(w, w.mintA, $(10), pythAccount(w.ctx, { price: 1_000_000_000n, age: -11 })), "PythPriceAhead");
    await expectError(post(w, w.mintA, $(10), pythAccount(w.ctx, { price: 1_000_000_000n, age: -86_400 })), "PythPriceAhead");
    // A few seconds of clock drift are fine.
    await post(w, w.mintA, $(10), pythAccount(w.ctx, { price: 1_000_000_000n, age: -10 }));
  });

  it("the price must be within 3% of Pyth's, or three times its confidence up to 5%", async () => {
    const w = await fed();
    const pyth = pythAccount(w.ctx, { price: 1_000_000_000n }); // $10.00, exponent -8
    await post(w, w.mintA, $(10.3), pyth);
    await expectError(post(w, w.mintA, $(10.31), pyth), "PriceOffPyth");
    await expectError(post(w, w.mintA, $(9.69), pyth), "PriceOffPyth");
    // Confidence of 1.5%: three times it is 4.5%.
    const wide = pythAccount(w.ctx, { price: 1_000_000_000n, conf: 15_000_000n });
    await post(w, w.mintA, $(10.45), wide);
    await expectError(post(w, w.mintA, $(10.46), wide), "PriceOffPyth");
    // Confidence of 4%: capped at 5%.
    const wider = pythAccount(w.ctx, { price: 1_000_000_000n, conf: 40_000_000n });
    await post(w, w.mintA, $(10.5), wider);
    await expectError(post(w, w.mintA, $(10.51), wider), "PriceOffPyth");
  });

  it("with a feed, Pyth replaces the band: a price far from the last one follows Pyth at once", async () => {
    const w = await fed();
    // Pyth at $13 (30% up), as when an asset's last price is days old: the price follows it.
    await post(w, w.mintA, $(13), pythAccount(w.ctx, { price: 1_300_000_000n }));
    assert.equal(await posted(w, w.mintA), $(13));
  });

  it("removing the feed brings the band back, from the last accepted price", async () => {
    const w = await fed();
    await post(w, w.mintA, $(13), pythAccount(w.ctx, { price: 1_300_000_000n }));
    await rpc(w.ctx, feedIx(w, w.mintA, Buffer.alloc(32)));
    warp(w.ctx, 3_601);
    await expectError(post(w, w.mintA, $(20)), "PriceMoveTooLarge");
  });

  it("removing the feed (all zeros) goes back to the band alone", async () => {
    const w = await fed();
    await rpc(w.ctx, feedIx(w, w.mintA, Buffer.alloc(32)));
    await post(w, w.mintA, $(11));
  });

  it("a scaled-UI token is priced per raw token: Pyth's price times the multiplier", async () => {
    const w = await priceWorld();
    // A Token-2022 mint whose wallets show 2x the raw amount (an xStock after a dividend).
    const mint = Keypair.generate();
    const space = 165 + 1 + 4 + 56;
    const multiplier = Buffer.alloc(8);
    multiplier.writeDoubleLE(2);
    await send(
      w.ctx,
      [
        SystemProgram.createAccount({ fromPubkey: w.ctx.payer.publicKey, newAccountPubkey: mint.publicKey, lamports: Number(w.ctx.svm.minimumBalanceForRentExemption(BigInt(space))), space, programId: TOKEN_2022_PROGRAM_ID }),
        new TransactionInstruction({
          programId: TOKEN_2022_PROGRAM_ID,
          keys: [{ pubkey: mint.publicKey, isSigner: false, isWritable: true }],
          data: Buffer.concat([Buffer.from([43, 0]), w.ctx.payer.publicKey.toBuffer(), multiplier]),
        }),
        createInitializeMint2Instruction(mint.publicKey, 8, w.ctx.payer.publicKey, null, TOKEN_2022_PROGRAM_ID),
      ],
      [mint],
    );
    await rpc(w.ctx, feedIx(w, mint.publicKey, FEED));
    const pyth = pythAccount(w.ctx, { price: 10_000_000_000n }); // $100 a token as wallets show it
    await expectError(post(w, mint.publicKey, $(100), pyth), "PriceOffPyth");
    await post(w, mint.publicKey, $(200), pyth);
  });

  it("an asset without a feed needs no Pyth account", async () => {
    const w = await priceWorld();
    const other = await createMint(w.ctx, 6);
    await post(w, other, 5n * E12);
  });
});
