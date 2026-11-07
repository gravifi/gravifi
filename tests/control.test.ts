// Who runs a basket now that holder governance is gone: its creator, or nobody (immutable).
// No basket can be created as a DAO, and one written as one (none exists on mainnet) is frozen:
// nothing changes it.
import { describe, it } from "node:test";
import { BN, expectError, fund, rpc } from "./helpers.ts";
import { world, type World } from "./fixtures.ts";

/** Rewrites the basket account as if it had been handed to its holders before the removal. */
async function makeLegacyDao(w: World) {
  const acct = w.ctx.svm.getAccount(w.basket)!;
  const decoded = w.ctx.vault.coder.accounts.decode("basket", Buffer.from(acct.data));
  decoded.control = { dao: {} };
  // Its governor's address sat in the bytes now used for the rebalance supply snapshot.
  const governor = fund(w.ctx).publicKey.toBuffer();
  decoded.rebalanceSupply = new BN(governor.readBigUInt64LE(0).toString());
  decoded.reserved = Array.from(governor.subarray(8, 22));
  decoded.creatorCutBps = governor.readUInt16LE(22);
  decoded.creatorUnclaimed = new BN(governor.readBigUInt64LE(24).toString());
  const data = await w.ctx.vault.coder.accounts.encode("basket", decoded);
  const full = Buffer.alloc(acct.data.length);
  data.copy(full);
  w.ctx.svm.setAccount(w.basket, { ...acct, data: full });
}

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
});
