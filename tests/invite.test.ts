// A referred wallet's first month: half the protocol fee on mint and redeem, all of it to the
// treasury, none burned. The wallet writes its invite with its first trade, paying the rent, and
// the keeper co-signs; the app passes it after the quads.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { BN, ONE_SHARE, balance, expectError, fund, now, pda, rpc, warp, type Ctx } from "./helpers.ts";
import { feeShares, fundHolder, mintIx, redeemIx, world, type World } from "./fixtures.ts";
import type { Keypair, PublicKey } from "@solana/web3.js";

const FEE_BPS = 40;
const SHARES = 10n * ONE_SHARE;
const DAY = 86_400;

async function setupInvites() {
  const w = await world({ protocolFeeBps: FEE_BPS });
  const keeper = fund(w.ctx);
  await rpc(w.ctx, w.ctx.vault.methods.initKeepers(keeper.publicKey, keeper.publicKey).accountsPartial({ authority: w.ctx.payer.publicKey, keepers: pda.keepers() }));
  const alice = fund(w.ctx);
  await fundHolder(w, alice);
  return { ...w, keeper, alice };
}

const setInvite = (ctx: Ctx, keeper: Keypair, wallet: Keypair, until: number) =>
  rpc(ctx, ctx.vault.methods.setInvite(new BN(until)).accountsPartial({ wallet: wallet.publicKey, rebalancer: keeper.publicKey, keepers: pda.keepers(), invite: pda.invite(wallet.publicKey) }), [wallet, keeper]);

/** Closed by whoever sends it: here the test's payer, a stranger to the invite. */
const closeInvite = (ctx: Ctx, wallet: PublicKey) => rpc(ctx, ctx.vault.methods.closeInvite().accountsPartial({ wallet, invite: pda.invite(wallet) }));

/** Anchor appends remaining accounts: this one lands after the quads. */
const plus = (builder: any, account: PublicKey) => builder.remainingAccounts([{ pubkey: account, isSigner: false, isWritable: false }]);

function fees(w: World) {
  const { burnShares, treasuryShares } = feeShares(w.shareMint, w.shareProgram);
  return { burn: balance(w.ctx, burnShares), treasury: balance(w.ctx, treasuryShares) };
}

/** Mint `shares` as `who`, with or without the invite after the quads; what the fee accounts gained. */
async function mint(w: World, who: Keypair, invite: boolean) {
  const before = fees(w);
  let b = mintIx(w, who, SHARES);
  if (invite) b = plus(b, pda.invite(who.publicKey));
  await rpc(w.ctx, b, [who]);
  const after = fees(w);
  return { burn: after.burn - before.burn, treasury: after.treasury - before.treasury };
}

describe("invites: a referred wallet's first month at half the protocol fee", () => {
  it("full fee without an invite: 0.40%, half burned", async () => {
    const w = await setupInvites();
    assert.deepEqual(await mint(w, w.alice, false), { burn: 20_000_000n, treasury: 20_000_000n });
  });

  it("with a live invite: 0.20%, all to the treasury, nothing burned; mint and redeem", async () => {
    const w = await setupInvites();
    await setInvite(w.ctx, w.keeper, w.alice, now(w.ctx) + 30 * DAY);
    assert.deepEqual(await mint(w, w.alice, true), { burn: 0n, treasury: 20_000_000n });

    const before = fees(w);
    let r = redeemIx(w, w.alice, 5n * ONE_SHARE);
    r = plus(r, pda.invite(w.alice.publicKey));
    await rpc(w.ctx, r, [w.alice]);
    const after = fees(w);
    assert.equal(after.burn - before.burn, 0n);
    assert.equal(after.treasury - before.treasury, 10_000_000n);
  });

  it("the wallet pays the invite's rent, and gets it back when anyone closes it after it ran out", async () => {
    const w = await setupInvites();
    const [aliceBefore, keeperBefore] = [w.ctx.svm.getBalance(w.alice.publicKey)!, w.ctx.svm.getBalance(w.keeper.publicKey)!];
    await setInvite(w.ctx, w.keeper, w.alice, now(w.ctx) + DAY);
    const rent = BigInt(w.ctx.svm.getAccount(pda.invite(w.alice.publicKey))!.lamports);
    assert.equal(w.ctx.svm.getBalance(w.keeper.publicKey), keeperBefore, "the keeper only co-signs");
    assert.ok(w.ctx.svm.getBalance(w.alice.publicKey)! <= aliceBefore - rent, "the wallet paid the rent");

    const stranger = fund(w.ctx);
    await expectError(closeInvite(w.ctx, w.alice.publicKey), "InviteActive");
    warp(w.ctx, DAY + 1);
    assert.deepEqual(await mint(w, w.alice, true), { burn: 20_000_000n, treasury: 20_000_000n });
    // Its rent goes only to its own wallet.
    await expectError(
      rpc(w.ctx, w.ctx.vault.methods.closeInvite().accountsPartial({ wallet: stranger.publicKey, invite: pda.invite(w.alice.publicKey) })),
      "BadInvite",
    );
    const aliceExpired = w.ctx.svm.getBalance(w.alice.publicKey)!;
    await closeInvite(w.ctx, w.alice.publicKey);
    assert.equal(w.ctx.svm.getAccount(pda.invite(w.alice.publicKey)), null);
    assert.equal(w.ctx.svm.getBalance(w.alice.publicKey), aliceExpired + rent);
  });

  it("the wallet's own invite address with nothing there yet charges the full fee", async () => {
    const w = await setupInvites();
    assert.deepEqual(await mint(w, w.alice, true), { burn: 20_000_000n, treasury: 20_000_000n });
    const bob = fund(w.ctx);
    await fundHolder(w, bob);
    // Someone else's empty address is still refused.
    await expectError(rpc(w.ctx, plus(mintIx(w, bob, SHARES), pda.invite(w.alice.publicKey)), [bob]), "BadInvite");
  });

  it("someone else's invite is refused, not taken as a discount", async () => {
    const w = await setupInvites();
    const bob = fund(w.ctx);
    await fundHolder(w, bob);
    await setInvite(w.ctx, w.keeper, w.alice, now(w.ctx) + 30 * DAY);
    await expectError(rpc(w.ctx, plus(mintIx(w, bob, SHARES), pda.invite(w.alice.publicKey)), [bob]), "BadInvite");
    // Nor an account that isn't an invite at all.
    await expectError(rpc(w.ctx, plus(mintIx(w, bob, SHARES), pda.keepers()), [bob]), "AccountDiscriminatorMismatch");
    // Nor two accounts after the quads.
    await expectError(rpc(w.ctx, plus(plus(mintIx(w, bob, SHARES), pda.invite(bob.publicKey)), pda.invite(bob.publicKey)), [bob]), "BadRemainingAccounts");
  });

  it("an invite needs the rebalancer's signature, at most a month and a day ahead, once per wallet", async () => {
    const w = await setupInvites();
    const mallory = fund(w.ctx);
    await expectError(setInvite(w.ctx, mallory, mallory, now(w.ctx) + DAY), "NotRebalancer");
    await expectError(setInvite(w.ctx, w.keeper, w.alice, now(w.ctx) + 32 * DAY), "BadInvite");
    await expectError(setInvite(w.ctx, w.keeper, w.alice, now(w.ctx) - 1), "BadInvite");
    await setInvite(w.ctx, w.keeper, w.alice, now(w.ctx) + 31 * DAY);
    await expectError(setInvite(w.ctx, w.keeper, w.alice, now(w.ctx) + DAY), "already in use");
  });
});
