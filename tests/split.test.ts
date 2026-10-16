// Share splits: new baskets have a Token-2022 share mint with a scaled UI amount, so a
// split only changes the display multiplier. Raw balances and unit math never move.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { BN, TOKEN_2022_PROGRAM_ID, ata, balance, expectError, fund, pda, rpc, supply, warp } from "./helpers.ts";

const MONTH = 30 * 86_400;
import { fundHolder, mintIx, redeemIx, world, type World } from "./fixtures.ts";

/** The multiplier in force: TLV entry of type 25 after the padded mint and type byte. */
function multiplier(w: World): number {
  const data = Buffer.from(w.ctx.svm.getAccount(w.shareMint)!.data);
  let i = 166;
  while (i + 4 <= data.length) {
    const type = data.readUInt16LE(i);
    const len = data.readUInt16LE(i + 2);
    if (type === 25) {
      const now = Number(w.ctx.svm.getClock().unixTimestamp);
      const effective = Number(data.readBigInt64LE(i + 4 + 40));
      return now >= effective ? data.readDoubleLE(i + 4 + 48) : data.readDoubleLE(i + 4 + 32);
    }
    i += 4 + len;
  }
  throw new Error("no scaled UI amount on this mint");
}

const splitIx = (w: World, signer = w.creator, numerator = 2, denominator = 1) =>
  w.ctx.vault.methods.split(numerator, denominator).accountsPartial({
    authority: signer.publicKey,
    basket: w.basket,
    shareMint: w.shareMint,
    tokenProgram: TOKEN_2022_PROGRAM_ID,
  });

describe("share splits", () => {
  it("new Token-2022 baskets start with a multiplier of 1", async () => {
    const w = await world({ shareTokenProgram: TOKEN_2022_PROGRAM_ID });
    assert.equal(multiplier(w), 1);
  });

  it("a 2:1 split doubles the multiplier and leaves raw balances and units alone", async () => {
    const w = await world({ shareTokenProgram: TOKEN_2022_PROGRAM_ID });
    const holderShares = ata(w.shareMint, w.creator.publicKey, TOKEN_2022_PROGRAM_ID);
    const [raw, units, total] = [balance(w.ctx, holderShares), (await w.ctx.vault.account.basket.fetch(w.basket)).constituents.map((c) => c.unitsPerShare.toString()), supply(w.ctx, w.shareMint)];
    await rpc(w.ctx, splitIx(w), [w.creator]);
    assert.equal(multiplier(w), 2);
    assert.equal(balance(w.ctx, holderShares), raw);
    assert.equal(supply(w.ctx, w.shareMint), total);
    assert.deepEqual((await w.ctx.vault.account.basket.fetch(w.basket)).constituents.map((c) => c.unitsPerShare.toString()), units);
  });

  it("splits compound, and a reverse split works too", async () => {
    const w = await world({ shareTokenProgram: TOKEN_2022_PROGRAM_ID });
    await rpc(w.ctx, splitIx(w, w.creator, 10, 1), [w.creator]);
    warp(w.ctx, MONTH);
    await rpc(w.ctx, splitIx(w, w.creator, 3, 1), [w.creator]);
    assert.equal(multiplier(w), 30);
    warp(w.ctx, MONTH);
    await rpc(w.ctx, splitIx(w, w.creator, 1, 30), [w.creator]);
    assert.equal(multiplier(w), 1);
  });

  it("at most one split, direct or reverse, every 30 days", async () => {
    const w = await world({ shareTokenProgram: TOKEN_2022_PROGRAM_ID });
    warp(w.ctx, 86_400); // the test clock starts at 0, which reads as "never split"
    await rpc(w.ctx, splitIx(w), [w.creator]);
    warp(w.ctx, MONTH - 60);
    await expectError(rpc(w.ctx, splitIx(w, w.creator, 1, 2), [w.creator]), "SplitTooSoon");
    warp(w.ctx, 60);
    await rpc(w.ctx, splitIx(w, w.creator, 1, 2), [w.creator]);
    assert.equal(multiplier(w), 1);
    w.ctx.svm.expireBlockhash();
    await expectError(rpc(w.ctx, splitIx(w, w.creator, 5, 1), [w.creator]), "SplitTooSoon");
  });

  it("minting and redeeming still work after a split", async () => {
    const w = await world({ shareTokenProgram: TOKEN_2022_PROGRAM_ID });
    await rpc(w.ctx, splitIx(w), [w.creator]);
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await rpc(w.ctx, mintIx(w, alice, 2_000_000_000n), [alice]);
    await rpc(w.ctx, redeemIx(w, alice, 2_000_000_000n), [alice]);
    assert.equal(balance(w.ctx, ata(w.shareMint, alice.publicKey, TOKEN_2022_PROGRAM_ID)), 0n);
  });

  it("only the creator can split a creator-managed basket", async () => {
    const w = await world({ shareTokenProgram: TOKEN_2022_PROGRAM_ID });
    const mallory = fund(w.ctx);
    await expectError(rpc(w.ctx, splitIx(w, mallory), [mallory]), "NotMandateAuthority");
  });

  it("an immutable basket can never split", async () => {
    const w = await world({ shareTokenProgram: TOKEN_2022_PROGRAM_ID, control: "immutable" });
    await expectError(rpc(w.ctx, splitIx(w), [w.creator]), "BasketImmutable");
  });

  it("ratios outside 1:100 to 100:1 are refused", async () => {
    const w = await world({ shareTokenProgram: TOKEN_2022_PROGRAM_ID });
    await expectError(rpc(w.ctx, splitIx(w, w.creator, 101, 1), [w.creator]), "BadParameter");
    await expectError(rpc(w.ctx, splitIx(w, w.creator, 5, 5), [w.creator]), "BadParameter");
  });

  it("a basket with a classic share mint can't split", async () => {
    const w = await world();
    await expectError(rpc(w.ctx, splitIx(w), [w.creator]), "SplitUnsupported");
  });
});
