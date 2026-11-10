// The protocol fee and the creator's cut in USDC (the team, 9 Nov 2026: "option 2"; "the fees to the
// creator ... should be in native currency, USDx but not shares"; "community basket fees go to holders
// staking the shares of the basket, those have to be paid in native as well").
//
// The price key vouches for a flow's dollar value (`fee_quote`, right before it in the same
// transaction); the flow takes the fee at the program's rates from the payer's USDC. Anything short
// of that — no value, no USDC, a wrong account — falls back to shares, as before: nothing that worked
// stops working, and no exit is ever blocked.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { Keypair, PublicKey, SYSVAR_INSTRUCTIONS_PUBKEY, type TransactionInstruction } from "@solana/web3.js";
import { MintLayout, TOKEN_PROGRAM_ID, createTransferInstruction } from "@solana/spl-token";
import { BN, ONE_SHARE, ata, balance, createAta, expectError, fund, mintTo, pda, rpc, send, supply, warp, type Ctx } from "./helpers.ts";
import { USDC, feeShares, fundHolder, initKeepers, mintIx, redeemIx, world, type World, type WorldOpts } from "./fixtures.ts";
import { feeBlock, quoteIx, usdcMint, usdcSides } from "./_usdc.ts";

const SHARES = 10n * ONE_SHARE;
const DOLLAR = 1_000_000n;

async function setup(opts: WorldOpts = {}) {
  const w = await world({ protocolFeeBps: 40, ...opts });
  usdcMint(w.ctx);
  const sides = await usdcSides(w.ctx);
  const priceKey = fund(w.ctx);
  await initKeepers(w.ctx, priceKey.publicKey, priceKey.publicKey);
  const alice = fund(w.ctx);
  await fundHolder(w, alice);
  const aliceUsdc = await createAta(w.ctx, USDC, alice.publicKey);
  await mintTo(w.ctx, USDC, aliceUsdc, 1_000n * DOLLAR);
  return { ...w, sides, priceKey, alice, aliceUsdc };
}
type S = Awaited<ReturnType<typeof setup>>;

const shareFees = (w: World) => {
  const { burnShares, treasuryShares } = feeShares(w.shareMint, w.shareProgram);
  return { burn: balance(w.ctx, burnShares), treasury: balance(w.ctx, treasuryShares) };
};
const usdcOf = (s: S) => ({ burn: balance(s.ctx, s.sides.burn), treasury: balance(s.ctx, s.sides.treasury), rewards: balance(s.ctx, s.sides.rewards), alice: balance(s.ctx, s.aliceUsdc) });
const sharesOf = (s: S, who: PublicKey) => balance(s.ctx, ata(s.shareMint, who, s.shareProgram));

/** A flow with the value signed right before it (or not), the fee block after the quads. */
async function flow(s: S, kind: "mint" | "redeem", value: bigint | null, opts: { shares?: bigint; min?: bigint; who?: Keypair; block?: any[] | null; signer?: Keypair; cut?: PublicKey; extra?: any[] } = {}) {
  const who = opts.who ?? s.alice;
  const shares = opts.shares ?? SHARES;
  let b = kind === "mint" ? mintIx(s, who, shares, null, opts.min) : redeemIx(s, who, shares);
  const block = opts.block === null ? [] : (opts.block ?? feeBlock(s.sides, who.publicKey, opts.cut));
  b = b.remainingAccounts([...block, ...(opts.extra ?? [])]);
  const ixs = [...(value === null ? [] : [await quoteIx(s.ctx, opts.signer ?? s.priceKey, value)]), await b.instruction()];
  const signers = [who, ...(value === null ? [] : [opts.signer ?? s.priceKey])];
  return send(s.ctx, ixs, signers);
}

describe("protocol fee in USDC", () => {
  it("a mint with a signed value pays 0.40% of it in USDC, half burned, and keeps no shares", async () => {
    const s = await setup();
    const before = { usdc: usdcOf(s), shares: shareFees(s) };
    await flow(s, "mint", 100n * DOLLAR);
    const after = { usdc: usdcOf(s), shares: shareFees(s) };
    assert.equal(sharesOf(s, s.alice.publicKey), SHARES, "every share minted is the buyer's");
    assert.deepEqual(after.shares, before.shares, "no fee shares");
    assert.equal(before.usdc.alice - after.usdc.alice, 400_000n, "$0.40");
    assert.equal(after.usdc.burn - before.usdc.burn, 200_000n);
    assert.equal(after.usdc.treasury - before.usdc.treasury, 200_000n);
    assert.equal(after.usdc.rewards, 0n, "staking off: nothing to stakers");
  });

  it("rounds up: no trade is too small to pay", async () => {
    const s = await setup();
    const before = usdcOf(s).alice;
    await flow(s, "mint", 10_000n, { shares: ONE_SHARE / 1000n });
    // $0.01 is 10,000 micro-USDC: 0.40% of it is 40; $0.0001 (100) would be 0.4, rounded up to 1.
    assert.equal(before - usdcOf(s).alice, 40n);
    const mid = usdcOf(s).alice;
    await flow(s, "mint", 100n, { shares: ONE_SHARE / 1000n });
    assert.equal(mid - usdcOf(s).alice, 1n);
  });

  it("a redeem with a signed value pays in USDC: all its shares burned, all paid out", async () => {
    const s = await setup();
    await flow(s, "mint", 100n * DOLLAR);
    const supplyBefore = supply(s.ctx, s.shareMint);
    const a = balance(s.ctx, ata(s.mintA, s.alice.publicKey));
    const fees = shareFees(s);
    const usdc = usdcOf(s).alice;
    await flow(s, "redeem", 100n * DOLLAR);
    assert.equal(supplyBefore - supply(s.ctx, s.shareMint), SHARES, "all burned");
    assert.deepEqual(shareFees(s), fees, "no fee shares");
    assert.equal(usdc - usdcOf(s).alice, 400_000n);
    assert.ok(balance(s.ctx, ata(s.mintA, s.alice.publicKey)) > a);
  });

  it("no signed value, no fee accounts, no USDC, no USDC account: the fee in shares, as before", async () => {
    for (const how of ["no value", "no block", "no usdc", "no account"] as const) {
      const s = await setup();
      if (how === "no usdc") await send(s.ctx, [createTransferInstruction(s.aliceUsdc, ata(USDC, s.creator.publicKey), s.alice.publicKey, 1_000n * DOLLAR)], [s.alice]).catch(async () => {
        await createAta(s.ctx, USDC, s.creator.publicKey);
        await send(s.ctx, [createTransferInstruction(s.aliceUsdc, ata(USDC, s.creator.publicKey), s.alice.publicKey, 1_000n * DOLLAR)], [s.alice]);
      });
      const fees = shareFees(s);
      const usdc = usdcOf(s).alice;
      const bob = how === "no account" ? fund(s.ctx) : s.alice;
      if (how === "no account") await fundHolder(s, bob);
      await flow(s, "mint", how === "no value" ? null : 100n * DOLLAR, { who: bob, block: how === "no block" ? null : undefined });
      const after = shareFees(s);
      assert.equal(after.burn - fees.burn, 20_000_000n, how);
      assert.equal(after.treasury - fees.treasury, 20_000_000n, how);
      assert.equal(usdcOf(s).alice, how === "no usdc" ? 0n : usdc, how);
      // And the way out: the same, never blocked.
      await flow(s, "redeem", how === "no value" ? null : 100n * DOLLAR, { who: bob, shares: 1n * ONE_SHARE, block: how === "no block" ? null : undefined });
    }
  });

  it("only the price key's value counts: anyone else's fails the transaction", async () => {
    const s = await setup();
    await expectError(flow(s, "mint", 1n, { signer: fund(s.ctx) }), "NotPriceAuthority");
  });

  it("a value must sit right before its flow: elsewhere it's ignored and the fee is in shares", async () => {
    const s = await setup();
    const fees = shareFees(s);
    const b = mintIx(s, s.alice, SHARES).remainingAccounts(feeBlock(s.sides, s.alice.publicKey));
    // The quote after the mint.
    await send(s.ctx, [await b.instruction(), await quoteIx(s.ctx, s.priceKey, 100n * DOLLAR)], [s.alice, s.priceKey]);
    assert.equal(shareFees(s).burn - fees.burn, 20_000_000n);
  });

  it("a redeem pays on the value signed for it", async () => {
    const s = await setup();
    await flow(s, "mint", 100n * DOLLAR);
    const fees = shareFees(s);
    const usdc = usdcOf(s).alice;
    await flow(s, "redeem", 50n * DOLLAR, { shares: 5n * ONE_SHARE });
    assert.equal(usdc - usdcOf(s).alice, 200_000n);
    assert.deepEqual(shareFees(s), fees);
  });

  it("a mint that comes out smaller than asked pays on its share of the value, rounded up", async () => {
    const s = await setup();
    // Ask for 20 shares, at least 5, holding enough for... everything: give bob just 10 shares' worth.
    const bob = fund(s.ctx);
    const h = await fundHolder(s, bob);
    // Leave bob half of what 20 shares take of token A.
    const needA = (balance(s.ctx, s.vaultA) * 20n * ONE_SHARE) / supply(s.ctx, s.shareMint);
    const hasA = balance(s.ctx, h.a);
    await send(s.ctx, [createTransferInstruction(h.a, ata(s.mintA, s.alice.publicKey), bob.publicKey, hasA - needA / 2n)], [bob]);
    const bobUsdc = await createAta(s.ctx, USDC, bob.publicKey);
    await mintTo(s.ctx, USDC, bobUsdc, 10n * DOLLAR);
    await flow(s, "mint", 200n * DOLLAR, { who: bob, shares: 20n * ONE_SHARE, min: 5n * ONE_SHARE });
    const minted = sharesOf(s, bob.publicKey);
    assert.ok(minted < 20n * ONE_SHARE && minted >= 5n * ONE_SHARE);
    const value = (200n * DOLLAR * minted + 20n * ONE_SHARE - 1n) / (20n * ONE_SHARE);
    assert.equal(10n * DOLLAR - balance(s.ctx, bobUsdc), (value * 40n + 9_999n) / 10_000n);
  });

  it("a burn, treasury or rewards account that isn't the canonical one: shares", async () => {
    const s = await setup();
    const fees = shareFees(s);
    const stray = Keypair.generate().publicKey;
    const wrong = { ...s.sides, burn: await createAta(s.ctx, USDC, stray) };
    await flow(s, "mint", 100n * DOLLAR, { block: feeBlock(wrong, s.alice.publicKey) });
    assert.equal(shareFees(s).burn - fees.burn, 20_000_000n);
  });

  it("smart baskets pay the Smart rate (0.80%) in USDC", async () => {
    const s = await setup();
    // The rate is picked by the basket's control (as protocol_cut does): flip it to Smart in place.
    const acct = s.ctx.svm.getAccount(s.basket)!;
    const b: any = await s.ctx.vault.account.basket.fetch(s.basket);
    b.control = { smart: {} };
    const enc: Buffer = await s.ctx.vault.coder.accounts.encode("basket", b);
    const data = Buffer.alloc(acct.data.length);
    enc.copy(data);
    s.ctx.svm.setAccount(s.basket, { ...acct, data });
    const usdc = usdcOf(s).alice;
    await flow(s, "mint", 100n * DOLLAR);
    assert.equal(usdc - usdcOf(s).alice, 800_000n);
  });

  it("a referred wallet pays half, all on the treasury's side", async () => {
    const s = await setup();
    const until = Number(s.ctx.svm.getClock().unixTimestamp) + 86_400;
    await rpc(s.ctx, s.ctx.vault.methods.setInvite(new BN(until)).accountsPartial({ wallet: s.alice.publicKey, rebalancer: s.priceKey.publicKey, keepers: pda.keepers(), invite: pda.invite(s.alice.publicKey) }), [s.alice, s.priceKey]);
    const before = usdcOf(s);
    await flow(s, "mint", 100n * DOLLAR, { extra: [{ pubkey: pda.invite(s.alice.publicKey), isSigner: false, isWritable: false }] });
    const after = usdcOf(s);
    assert.equal(before.alice - after.alice, 200_000n);
    assert.equal(after.burn - before.burn, 0n);
    assert.equal(after.treasury - before.treasury, 200_000n);
  });

  it("with staking on, the stakers' 10% goes to their rewards PDA in USDC", async () => {
    const s = await setup();
    await rpc(s.ctx, s.ctx.vault.methods.initStaking().accountsPartial({ authority: s.ctx.payer.publicKey, protocol: pda.protocol(), graviMint: s.graviMint, pool: pda.stakePool() }));
    const before = usdcOf(s);
    await flow(s, "mint", 100n * DOLLAR);
    const after = usdcOf(s);
    assert.equal(after.burn - before.burn, 200_000n);
    assert.equal(after.rewards - before.rewards, 40_000n);
    assert.equal(after.treasury - before.treasury, 160_000n);
    const b = await s.ctx.vault.account.basket.fetch(s.basket);
    assert.equal(b.stakeOwed.toString(), "0", "nothing owed in shares");
  });

  it("the treasury's USDC: only the protocol authority moves it", async () => {
    const s = await setup();
    await flow(s, "mint", 100n * DOLLAR);
    const to = await createAta(s.ctx, USDC, s.creator.publicKey);
    const withdraw = (who: Keypair, amount: bigint) =>
      rpc(s.ctx, s.ctx.vault.methods.treasuryWithdrawUsdc(new BN(amount.toString())).accountsPartial({ authority: who.publicKey, protocol: pda.protocol(), usdcMint: USDC, treasuryUsdc: s.sides.treasury, destination: to, tokenProgram: TOKEN_PROGRAM_ID }), [who]);
    await expectError(withdraw(s.alice, 1n), "NotProtocolAuthority");
    await withdraw(s.ctx.payer, 200_000n);
    assert.equal(balance(s.ctx, to), 200_000n);
    assert.equal(balance(s.ctx, s.sides.treasury), 0n);
  });
});

describe("the creator's cut in USDC", () => {
  async function withCut(cutBps = 60) {
    const s = await setup();
    // A cut rise on a basket with shares out: a day's notice, then applied.
    await rpc(s.ctx, s.ctx.vault.methods.proposeCutRise(cutBps).accountsPartial({ authority: s.creator.publicKey, basket: s.basket, notice: pda.cutNotice(s.basket) }), [s.creator]);
    warp(s.ctx, 86_401);
    await rpc(s.ctx, s.ctx.vault.methods.applyCutRise().accountsPartial({ basket: s.basket, notice: pda.cutNotice(s.basket), creator: s.creator.publicKey }));
    const creatorUsdc = await createAta(s.ctx, USDC, s.creator.publicKey);
    return { ...s, creatorUsdc };
  }

  it("goes straight to the manager's USDC account: nothing owed, nothing in shares", async () => {
    const s = await withCut();
    const fees = shareFees(s);
    const usdc = usdcOf(s).alice;
    await flow(s, "mint", 100n * DOLLAR, { cut: s.creatorUsdc });
    assert.equal(balance(s.ctx, s.creatorUsdc), 600_000n, "0.60% of $100");
    assert.equal(usdc - usdcOf(s).alice, 1_000_000n, "0.40% + 0.60%");
    assert.equal(sharesOf(s, s.alice.publicKey), SHARES);
    assert.deepEqual(shareFees(s), fees);
    const b = await s.ctx.vault.account.basket.fetch(s.basket);
    assert.equal(b.creatorUnclaimed.toString(), "0");
  });

  it("an account that isn't the manager's: the cut in shares, the protocol fee still in USDC", async () => {
    const s = await withCut();
    const fees = shareFees(s);
    await flow(s, "mint", 100n * DOLLAR, { cut: s.aliceUsdc });
    const cut = (SHARES * 60n + 9_999n) / 10_000n;
    assert.equal(shareFees(s).treasury - fees.treasury, cut);
    assert.equal(sharesOf(s, s.alice.publicKey), SHARES - cut);
    const b = await s.ctx.vault.account.basket.fetch(s.basket);
    assert.equal(b.creatorUnclaimed.toString(), cut.toString());
    assert.equal(balance(s.ctx, s.creatorUsdc), 0n);
  });

  it("USDC enough for the fee only: the fee in USDC, the cut in shares", async () => {
    const s = await withCut();
    await send(s.ctx, [createTransferInstruction(s.aliceUsdc, s.creatorUsdc, s.alice.publicKey, 1_000n * DOLLAR - 500_000n)], [s.alice]);
    const paid = balance(s.ctx, s.creatorUsdc);
    await flow(s, "mint", 100n * DOLLAR, { cut: s.creatorUsdc });
    assert.equal(balance(s.ctx, s.aliceUsdc), 100_000n, "$0.40 fee taken");
    assert.equal(balance(s.ctx, s.creatorUsdc), paid, "no cut in USDC");
  });

  it("the manager's own mint pays no cut", async () => {
    const s = await withCut();
    const creatorHolder = s.creator;
    await mintTo(s.ctx, USDC, s.creatorUsdc, 10n * DOLLAR);
    await flow(s, "mint", 100n * DOLLAR, { who: creatorHolder, cut: s.creatorUsdc });
    assert.equal(balance(s.ctx, s.creatorUsdc), 10n * DOLLAR - 400_000n);
  });

  it("a redeem never pays a cut", async () => {
    const s = await withCut();
    await flow(s, "mint", 100n * DOLLAR, { cut: s.creatorUsdc });
    const paid = balance(s.ctx, s.creatorUsdc);
    await flow(s, "redeem", 100n * DOLLAR, { cut: s.creatorUsdc });
    assert.equal(balance(s.ctx, s.creatorUsdc), paid);
  });
});
