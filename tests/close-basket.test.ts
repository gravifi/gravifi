// Closing an emptied basket (`close_vaults`, then `close_basket`): once every share is gone, its
// manager gets back the rent of its vaults, the basket account and its side accounts.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, ata, balance, createAta, expectError, fund, mintTo, pda, rpc, supply } from "./helpers.ts";
import { SEED_SHARES, feeShares, redeemIx, world, type World, type WorldOpts } from "./fixtures.ts";
import { acct, closeBasket, closeVaults, gone, lamports, vaultQuads } from "./_close.ts";

/** The Basket account's `reserved[4]` flag byte, as hand-over.test.ts pokes it. */
const RESERVED4_AT = 94;
const TRIP_OPEN = 8;
const TRIP_STUCK = 16;

/** A basket whose creator has redeemed every share. */
async function emptied(opts: WorldOpts = {}) {
  const w = await world({ creatorFeeBps: 0, ...opts });
  await rpc(w.ctx, redeemIx(w, w.creator, SEED_SHARES), [w.creator]);
  assert.equal(supply(w.ctx, w.shareMint), 0n);
  return w;
}

function setFlag(w: World, bit: number) {
  const a = w.ctx.svm.getAccount(w.basket)!;
  const data = Buffer.from(a.data);
  data[RESERVED4_AT] |= bit;
  w.ctx.svm.setAccount(w.basket, { ...a, data });
}

describe("closing an emptied basket", () => {
  it("is refused while any share is out", async () => {
    const w = await world({ creatorFeeBps: 0 });
    await expectError(closeVaults(w, w.creator), "BasketNotEmpty");
    await rpc(w.ctx, redeemIx(w, w.creator, SEED_SHARES - 1n), [w.creator]);
    await expectError(closeVaults(w, w.creator), "BasketNotEmpty");
    await expectError(closeBasket(w, w.creator), "BasketNotEmpty");
  });

  it("is the manager's alone", async () => {
    const w = await emptied();
    const eve = fund(w.ctx);
    await createAta(w.ctx, w.mintA, eve.publicKey, TOKEN_PROGRAM_ID);
    await createAta(w.ctx, w.mintB, eve.publicKey, TOKEN_2022_PROGRAM_ID);
    await expectError(closeVaults(w, eve), "NotMandateAuthority");
    await expectError(closeBasket(w, eve), "NotMandateAuthority");
  });

  it("waits for a trip that's out or stuck", async () => {
    for (const bit of [TRIP_OPEN, TRIP_STUCK]) {
      const w = await emptied();
      setFlag(w, bit);
      await expectError(closeVaults(w, w.creator), "TripOpen");
      await expectError(closeBasket(w, w.creator), "TripOpen");
    }
  });

  it("closes the vaults, then the basket and its side accounts, every lamport of rent to the manager", async () => {
    const w = await emptied({ access: "allowlist" });
    const c = w.creator;
    const friend = fund(w.ctx).publicKey;
    await rpc(w.ctx, w.ctx.vault.methods.allowWallet().accountsPartial({ creator: c.publicKey, basket: w.basket, wallet: friend, allowEntry: pda.allow(w.basket, friend) }), [c]);
    await rpc(w.ctx, w.ctx.vault.methods.setStops([]).accountsPartial({ creator: c.publicKey, basket: w.basket, stopLoss: pda.stop(w.basket) }), [c]);
    const side = [pda.allow(w.basket, friend), pda.stop(w.basket)];
    for (const k of side) assert.ok(lamports(w, k) > 0n);

    // The basket isn't closable while a vault is open.
    await expectError(closeBasket(w, c, side), "VaultsOpen");

    // Dust someone sent after the last redeem goes to the manager with the vault's rent.
    await mintTo(w.ctx, w.mintA, w.vaultA, 12_345n, TOKEN_PROGRAM_ID);
    const aBefore = balance(w.ctx, ata(w.mintA, c.publicKey, TOKEN_PROGRAM_ID));
    const rentVaults = lamports(w, w.vaultA) + lamports(w, w.vaultB);
    const solBefore = lamports(w, c.publicKey);
    await closeVaults(w, c);
    assert.ok(gone(w, w.vaultA) && gone(w, w.vaultB));
    assert.equal(balance(w.ctx, ata(w.mintA, c.publicKey, TOKEN_PROGRAM_ID)) - aBefore, 12_345n);
    const fee = 0n; // the provider wallet pays the fees in these tests
    assert.equal(lamports(w, c.publicKey) - solBefore, rentVaults - fee);

    // A stranger's account among the side accounts is refused.
    await expectError(closeBasket(w, c, [...side, pda.protocol()]), "BadRemainingAccounts");
    await expectError(closeBasket(w, c, [...side, fund(w.ctx).publicKey]), "BadRemainingAccounts");

    const { treasuryShares, burnShares } = feeShares(w.shareMint, w.shareProgram);
    const back = [w.basket, ...side, treasuryShares, burnShares].reduce((s, k) => s + lamports(w, k), 0n);
    const before = lamports(w, c.publicKey);
    await closeBasket(w, c, side);
    for (const k of [w.basket, ...side, treasuryShares, burnShares]) assert.ok(gone(w, k), `${k.toBase58()} closed`);
    assert.equal(lamports(w, c.publicKey) - before, back - fee);
    // The share mint has no close authority: it stays, with no supply.
    assert.ok(lamports(w, w.shareMint) > 0n);
  });

  it("takes only the canonical vaults, and only the vaults' own token program", async () => {
    const w = await emptied();
    const c = w.creator;
    const q = vaultQuads(w, c.publicKey);
    // Asset B's vault named with the classic token program: refused.
    const wrong = [...q.slice(0, 4), q[4], q[5], q[6], acct(TOKEN_PROGRAM_ID, false)];
    await expectError(closeVaults(w, c, wrong), "ConstituentMismatch");
    // A token account the basket owns that isn't its vault for the mint: refused.
    const side = await createAta(w.ctx, w.mintA, fund(w.ctx).publicKey, TOKEN_PROGRAM_ID);
    await expectError(closeVaults(w, c, [acct(side), q[1], q[2], q[3]]), "NotBasketVault");
    // One at a time works.
    await closeVaults(w, c, q.slice(0, 4));
    await expectError(closeBasket(w, c), "VaultsOpen");
    await closeVaults(w, c, q.slice(4));
    // Without the fee accounts named, they stay; the basket still closes.
    await closeBasket(w, c, [], false);
    assert.ok(gone(w, w.basket));
    assert.ok(lamports(w, feeShares(w.shareMint, w.shareProgram).treasuryShares) > 0n);
  });

  it("a vault reopened by anyone can be closed again", async () => {
    const w = await emptied();
    await closeVaults(w, w.creator);
    await createAta(w.ctx, w.mintA, w.basket, TOKEN_PROGRAM_ID);
    await expectError(closeBasket(w, w.creator), "VaultsOpen");
    await closeVaults(w, w.creator, vaultQuads(w, w.creator.publicKey).slice(0, 4));
    await closeBasket(w, w.creator);
    assert.ok(gone(w, w.basket));
  });

  it("pays a handed-over basket's rent to whoever it was handed to", async () => {
    const w = await emptied({ shareTokenProgram: TOKEN_2022_PROGRAM_ID });
    const bob = fund(w.ctx);
    await rpc(w.ctx, w.ctx.vault.methods.handOver(bob.publicKey).accountsPartial({ manager: w.creator.publicKey, basket: w.basket }), [w.creator]);
    await expectError(closeVaults(w, w.creator), "NotMandateAuthority");
    await createAta(w.ctx, w.mintA, bob.publicKey, TOKEN_PROGRAM_ID);
    await createAta(w.ctx, w.mintB, bob.publicKey, TOKEN_2022_PROGRAM_ID);
    await closeVaults(w, bob);
    const before = lamports(w, bob.publicKey);
    const rent = lamports(w, w.basket);
    await closeBasket(w, bob);
    assert.ok(lamports(w, bob.publicKey) - before >= rent - 5_000n);
    assert.ok(gone(w, w.basket));
  });
});
