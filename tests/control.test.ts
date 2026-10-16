// Who runs a basket now that holder governance is gone: its creator, or nobody (immutable).
// No basket can be created as a DAO, and one that was handed to its holders before the
// removal can be returned to its creator by the protocol authority, and only such a basket.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { BN, expectError, fund, pda, rpc } from "./helpers.ts";
import { world, type World } from "./fixtures.ts";

/** Rewrites the basket account as if it had been handed to its holders before the removal. */
async function makeLegacyDao(w: World) {
  const acct = w.ctx.svm.getAccount(w.basket)!;
  const decoded = w.ctx.vault.coder.accounts.decode("basket", Buffer.from(acct.data));
  decoded.control = { dao: {} };
  // Its governor's address sat in the bytes now used for the rebalance supply snapshot.
  const governor = fund(w.ctx).publicKey.toBuffer();
  decoded.rebalanceSupply = new BN(governor.readBigUInt64LE(0).toString());
  decoded.reserved = Array.from(governor.subarray(8));
  const data = await w.ctx.vault.coder.accounts.encode("basket", decoded);
  const full = Buffer.alloc(acct.data.length);
  data.copy(full);
  w.ctx.svm.setAccount(w.basket, { ...acct, data: full });
}

const restoreIx = (w: World, authority = w.ctx.payer) =>
  w.ctx.vault.methods.restoreCreatorControl().accountsPartial({
    authority: authority.publicKey,
    protocol: pda.protocol(),
    basket: w.basket,
  });

const setTargetsIx = (w: World) =>
  w.ctx.vault.methods.setTargets([5_000, 5_000]).accountsPartial({ authority: w.creator.publicKey, basket: w.basket });

describe("basket control without holder governance", () => {
  it("refuses to create a DAO basket", async () => {
    await expectError(world({ control: "dao" }), "GovernanceRemoved");
  });

  it("a legacy DAO basket is frozen: not even its creator can change it", async () => {
    const w = await world();
    await makeLegacyDao(w);
    await expectError(rpc(w.ctx, setTargetsIx(w), [w.creator]), "GovernanceRemoved");
  });

  it("the protocol authority returns a legacy DAO basket to its creator", async () => {
    const w = await world();
    await makeLegacyDao(w);
    await rpc(w.ctx, restoreIx(w));
    const b = await w.ctx.vault.account.basket.fetch(w.basket);
    assert.deepEqual(b.control, { creator: {} });
    assert.equal(b.rebalanceSupply.toString(), "0");
    assert.deepEqual(b.reserved, new Array(24).fill(0));
    await rpc(w.ctx, setTargetsIx(w), [w.creator]);
    const after = await w.ctx.vault.account.basket.fetch(w.basket);
    assert.deepEqual(after.constituents.map((c) => c.targetBps), [5_000, 5_000]);
  });

  it("only the protocol authority can restore, and only a DAO basket", async () => {
    const w = await world();
    await expectError(rpc(w.ctx, restoreIx(w)), "NotDaoBasket");
    await makeLegacyDao(w);
    const stranger = fund(w.ctx);
    await expectError(rpc(w.ctx, restoreIx(w, stranger), [stranger]), "NotProtocolAuthority");
  });
});
