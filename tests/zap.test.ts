// Buying with swaps: the wallet signs the mint before its swaps have filled, so it asks for
// the quoted shares with a slippage floor and the vault mints what the balances cover.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, ata, balance, createAta, expectError, fund, mintTo, rpc } from "./helpers.ts";
import { mintIx, world } from "./fixtures.ts";

// World units: 2 A (6 decimals) and 0.5 B (9 decimals) per share.
async function bobWith(a: bigint, b: bigint) {
  const w = await world();
  const bob = fund(w.ctx);
  await mintTo(w.ctx, w.mintA, await createAta(w.ctx, w.mintA, bob.publicKey, TOKEN_PROGRAM_ID), a, TOKEN_PROGRAM_ID);
  await mintTo(w.ctx, w.mintB, await createAta(w.ctx, w.mintB, bob.publicKey, TOKEN_2022_PROGRAM_ID), b, TOKEN_2022_PROGRAM_ID);
  await createAta(w.ctx, w.shareMint, bob.publicKey);
  return { w, bob };
}

describe("minting what the swaps delivered", () => {
  it("mints the most shares the balances cover, up to the quote, and uses them up", async () => {
    // Just over 3 shares of A, and B for exactly 3.000000014 shares: B is the binding leg.
    const { w, bob } = await bobWith(6_000_005n, 1_500_000_007n);
    await rpc(w.ctx, mintIx(w, bob, 4_000_000_000n, null, 2_900_000_000n), [bob]);
    assert.equal(balance(w.ctx, ata(w.shareMint, bob.publicKey)), 3_000_000_014n);
    assert.equal(balance(w.ctx, ata(w.mintB, bob.publicKey, TOKEN_2022_PROGRAM_ID)), 0n, "the binding leg is used up");
    assert.equal(balance(w.ctx, ata(w.mintA, bob.publicKey)), 4n, "the other leaves only dust");
  });

  it("never mints more than asked, whatever the wallet holds", async () => {
    const { w, bob } = await bobWith(100_000_000n, 100_000_000_000n);
    await rpc(w.ctx, mintIx(w, bob, 2_000_000_000n, null, 1_000_000_000n), [bob]);
    assert.equal(balance(w.ctx, ata(w.shareMint, bob.publicKey)), 2_000_000_000n);
  });

  it("refuses when the swaps came back below the slippage floor", async () => {
    const { w, bob } = await bobWith(6_000_000n, 1_500_000_000n);
    await expectError(rpc(w.ctx, mintIx(w, bob, 4_000_000_000n, null, 3_500_000_000n), [bob]), "BelowMinShares");
  });

  it("a floor above the ask is refused", async () => {
    const { w, bob } = await bobWith(6_000_000n, 1_500_000_000n);
    await expectError(rpc(w.ctx, mintIx(w, bob, 1_000_000_000n, null, 2_000_000_000n), [bob]), "BadParameter");
  });
});
