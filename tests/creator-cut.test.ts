// The creator's cut: at most 0.60% of every mint by someone else, in place of the yearly fee. It
// waits in the treasury's shares, counted on the basket; redeems never pay it, and a rise on a
// basket with shares out waits a day.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { BN, ONE_SHARE, TOKEN_2022_PROGRAM_ID, ata, balance, createAta, expectError, fund, now, pda, rpc, supply, warp, type Ctx } from "./helpers.ts";
import { feeShares, fundHolder, mintIx, redeemIx, world, type World, type WorldOpts, initKeepers, setParam, PARAM, PARAM_NOTICE } from "./fixtures.ts";
import type { Keypair, PublicKey } from "@solana/web3.js";

const SHARES = 10n * ONE_SHARE;
const DAY = 86_400;

/** A seeded basket at the 0.40% protocol fee, its yearly fee as `creatorFeeBps`, with one buyer. */
async function setupCut(opts: WorldOpts = {}) {
  const w = await world({ protocolFeeBps: 40, ...opts });
  const alice = fund(w.ctx);
  await fundHolder(w, alice);
  return { ...w, alice };
}

const fetch = (w: World) => w.ctx.vault.account.basket.fetch(w.basket);

const setCut = (w: World, bps: number, who: Keypair = w.creator, notice: PublicKey | null = null) =>
  rpc(w.ctx, w.ctx.vault.methods.setCreatorCut(bps).accountsPartial({ authority: who.publicKey, basket: w.basket, notice }), [who]);

const propose = (w: World, bps: number, who: Keypair = w.creator) =>
  rpc(w.ctx, w.ctx.vault.methods.proposeCutRise(bps).accountsPartial({ authority: who.publicKey, basket: w.basket, notice: pda.cutNotice(w.basket) }), [who]);

const apply = (w: World, creator: PublicKey = w.creator.publicKey) =>
  rpc(w.ctx, w.ctx.vault.methods.applyCutRise().accountsPartial({ basket: w.basket, notice: pda.cutNotice(w.basket), creator }));

const claim = (w: World, creatorShares: PublicKey = ata(w.shareMint, w.creator.publicKey, w.shareProgram)) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.claimCreatorCut().accountsPartial({
      basket: w.basket,
      protocol: pda.protocol(),
      shareMint: w.shareMint,
      treasuryShares: feeShares(w.shareMint, w.shareProgram).treasuryShares,
      creatorShares,
      shareTokenProgram: w.shareProgram,
    }),
  );

const withdraw = (w: World, amount: bigint, destination: PublicKey) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.treasuryWithdraw(new BN(amount.toString())).accountsPartial({
      authority: w.ctx.payer.publicKey,
      protocol: pda.protocol(),
      basket: w.basket,
      shareMint: w.shareMint,
      treasuryShares: feeShares(w.shareMint, w.shareProgram).treasuryShares,
      destination,
      shareTokenProgram: w.shareProgram,
    }),
  );

function fees(w: World) {
  const { burnShares, treasuryShares } = feeShares(w.shareMint, w.shareProgram);
  return { burn: balance(w.ctx, burnShares), treasury: balance(w.ctx, treasuryShares) };
}

/** What `who` got and the fee accounts gained from minting `SHARES`. */
async function mint(w: World, who: Keypair) {
  const before = fees(w);
  const held = balance(w.ctx, ata(w.shareMint, who.publicKey, w.shareProgram));
  await rpc(w.ctx, mintIx(w, who, SHARES), [who]);
  const after = fees(w);
  return { got: balance(w.ctx, ata(w.shareMint, who.publicKey, w.shareProgram)) - held, burn: after.burn - before.burn, treasury: after.treasury - before.treasury };
}

/** A cut that's already on: a day's notice, applied. */
async function cutOn(w: World, bps: number) {
  await propose(w, bps);
  warp(w.ctx, DAY + 1);
  await apply(w);
}

describe("creator cut: what a mint pays", () => {
  it("a mint pays the cut on top of the protocol fee, and the burn's part doesn't move", async () => {
    const w = await setupCut();
    await cutOn(w, 60);
    assert.deepEqual(await mint(w, w.alice), { got: 9_900_000_000n, burn: 20_000_000n, treasury: 80_000_000n });
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), "60000000");
  });

  it("a basket that never set one pays exactly today's fee", async () => {
    const w = await setupCut();
    assert.deepEqual(await mint(w, w.alice), { got: 9_960_000_000n, burn: 20_000_000n, treasury: 20_000_000n });
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), "0");
  });

  it("no new shares come from nowhere: the deposit covers the cut, so the share's slice holds", async () => {
    const w = await setupCut();
    await cutOn(w, 60);
    const [vault, sup] = [balance(w.ctx, w.vaultA), supply(w.ctx, w.shareMint)];
    await mint(w, w.alice);
    const [vaultAfter, supAfter] = [balance(w.ctx, w.vaultA), supply(w.ctx, w.shareMint)];
    assert.equal(supAfter - sup, SHARES);
    assert.equal((vaultAfter * sup) / supAfter, vault);
  });

  it("a redeem pays only the protocol fee", async () => {
    const w = await setupCut();
    await cutOn(w, 60);
    await mint(w, w.alice);
    const owed = (await fetch(w)).creatorUnclaimed.toString();
    const before = fees(w);
    await rpc(w.ctx, redeemIx(w, w.alice, 5n * ONE_SHARE), [w.alice]);
    const after = fees(w);
    assert.equal(after.burn - before.burn, 10_000_000n);
    assert.equal(after.treasury - before.treasury, 10_000_000n);
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), owed);
  });

  it("the creator's own mint and seed pay none", async () => {
    const w = await setupCut({ seed: false });
    await setCut(w, 60);
    const { rpcSeed } = await seedOf(w);
    await rpcSeed();
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), "0");
    await mint(w, w.creator);
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), "0");
  });

  it("an invited wallet pays half the protocol fee and the whole cut", async () => {
    const w = await setupCut();
    await cutOn(w, 60);
    const keeper = fund(w.ctx);
    await initKeepers(w.ctx, keeper.publicKey, keeper.publicKey);
    await rpc(
      w.ctx,
      w.ctx.vault.methods.setInvite(new BN(now(w.ctx) + DAY)).accountsPartial({ wallet: w.alice.publicKey, rebalancer: keeper.publicKey, keepers: pda.keepers(), invite: pda.invite(w.alice.publicKey) }),
      [w.alice, keeper],
    );
    const before = fees(w);
    const b = mintIx(w, w.alice, SHARES).remainingAccounts([{ pubkey: pda.invite(w.alice.publicKey), isSigner: false, isWritable: false }]);
    await rpc(w.ctx, b, [w.alice]);
    const after = fees(w);
    assert.equal(after.burn - before.burn, 0n);
    assert.equal(after.treasury - before.treasury, 80_000_000n);
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), "60000000");
  });

  it("rounds up, so small mints can't dodge it", async () => {
    const w = await setupCut();
    await cutOn(w, 60);
    await rpc(w.ctx, mintIx(w, w.alice, 1_000n), [w.alice]);
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), "6");
    await rpc(w.ctx, mintIx(w, w.alice, 10n), [w.alice]);
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), "7");
  });

  it("a mint too small to cover both refuses", async () => {
    const w = await setupCut();
    await cutOn(w, 60);
    await expectError(rpc(w.ctx, mintIx(w, w.alice, 2n), [w.alice]), "BelowFee");
  });
});

describe("creator cut: setting it", () => {
  it("is capped at 0.60%", async () => {
    const w = await setupCut({ seed: false });
    await expectError(setCut(w, 61), "FeeTooHigh");
    await setCut(w, 60);
    assert.equal((await fetch(w)).creatorCutBps, 60);
    const seeded = await setupCut();
    await expectError(propose(seeded, 61), "FeeTooHigh");
  });

  it("only the creator sets or raises it", async () => {
    const w = await setupCut();
    const stranger = fund(w.ctx);
    await expectError(setCut(w, 0, stranger), "NotMandateAuthority");
    await expectError(propose(w, 30, stranger), "NotMandateAuthority");
  });

  it("before the first deposit it applies at once, and stops the yearly fee", async () => {
    const w = await setupCut({ seed: false, creatorFeeBps: 15 });
    await setCut(w, 30);
    const b = await fetch(w);
    assert.equal(b.creatorCutBps, 30);
    assert.equal(b.mgmtFeeBps, 0);
  });

  it("with shares out a rise waits a day; anyone applies it, and the rent goes back to the creator", async () => {
    const w = await setupCut({ creatorFeeBps: 15 });
    await expectError(setCut(w, 30), "CutRiseNeedsNotice");
    await propose(w, 30);
    const notice = w.ctx.svm.getAccount(pda.cutNotice(w.basket))!;
    assert.ok(notice.lamports > 0);
    let b = await fetch(w);
    assert.equal(b.creatorCutBps, 0, "not yet");
    assert.equal(b.mgmtFeeBps, 0, "the yearly fee stops on asking");

    await expectError(apply(w), "CutNoticeRunning");
    assert.deepEqual(await mint(w, w.alice), { got: 9_960_000_000n, burn: 20_000_000n, treasury: 20_000_000n }, "a buyer during the notice pays the old cut");

    warp(w.ctx, DAY + 1);
    const before = w.ctx.svm.getBalance(w.creator.publicKey)!;
    await expectError(apply(w, w.alice.publicKey), "NotMandateAuthority");
    await apply(w);
    b = await fetch(w);
    assert.equal(b.creatorCutBps, 30);
    assert.equal(w.ctx.svm.getBalance(w.creator.publicKey)!, before + BigInt(notice.lamports));
    assert.equal(w.ctx.svm.getAccount(pda.cutNotice(w.basket))?.lamports ?? 0, 0);
  });

  it("going down applies at once", async () => {
    const w = await setupCut();
    await cutOn(w, 60);
    await setCut(w, 10);
    assert.equal((await fetch(w)).creatorCutBps, 10);
    await setCut(w, 0);
    assert.deepEqual(await mint(w, w.alice), { got: 9_960_000_000n, burn: 20_000_000n, treasury: 20_000_000n });
  });

  it("a rise must be a rise, one at a time, and passing the notice cancels it", async () => {
    const w = await setupCut();
    await cutOn(w, 30);
    await expectError(propose(w, 30), "BadParameter");
    await propose(w, 40);
    await expectError(propose(w, 50), "already in use");
    await setCut(w, 30, w.creator, pda.cutNotice(w.basket));
    assert.equal(w.ctx.svm.getAccount(pda.cutNotice(w.basket))?.lamports ?? 0, 0);
    warp(w.ctx, DAY + 1);
    await expectError(apply(w), "AccountNotInitialized");
    await propose(w, 50);
    assert.equal((await fetch(w)).creatorCutBps, 30);
  });

  it("a lowering calls off a rise asked for before it, notice passed or not (bug bounty, 7 Nov)", async () => {
    const w = await setupCut();
    await cutOn(w, 50);
    await propose(w, 60);
    // Lowered without the notice: before the fix, the rise then applied over the lowered cut.
    await setCut(w, 10);
    warp(w.ctx, DAY + 1);
    await expectError(apply(w), "CutRiseVoid");
    assert.equal((await fetch(w)).creatorCutBps, 10);
    // The creator closes the stale notice with the next set, rent back, and may ask again: a new
    // rise waits its own day and applies.
    await setCut(w, 10, w.creator, pda.cutNotice(w.basket));
    assert.equal(w.ctx.svm.getAccount(pda.cutNotice(w.basket))?.lamports ?? 0, 0);
    await propose(w, 20);
    await expectError(apply(w), "CutNoticeRunning");
    warp(w.ctx, DAY + 1);
    await apply(w);
    assert.equal((await fetch(w)).creatorCutBps, 20);
  });

  it("setting the same cut also calls off a rise waiting", async () => {
    const w = await setupCut();
    await cutOn(w, 30);
    await propose(w, 60);
    await setCut(w, 30);
    warp(w.ctx, DAY + 1);
    await expectError(apply(w), "CutRiseVoid");
    assert.equal((await fetch(w)).creatorCutBps, 30);
  });

  it("a basket has the yearly fee or the cut, never both", async () => {
    const w = await setupCut({ creatorFeeBps: 0 });
    await cutOn(w, 30);
    const policy = (fee: number) =>
      rpc(w.ctx, w.ctx.vault.methods.setPolicy({ hybrid: {} }, new BN(30 * DAY), 500, fee).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }), [w.creator]);
    await expectError(policy(15), "FeeTooHigh");
    await policy(0);
    await setCut(w, 0);
    await policy(15);
    assert.equal((await fetch(w)).mgmtFeeBps, 15);
    // Asking for a cut stops it again.
    await propose(w, 30);
    assert.equal((await fetch(w)).mgmtFeeBps, 0);
  });

  it("an immutable basket fixes it before its first deposit", async () => {
    const w = await setupCut({ control: "immutable", seed: false });
    await setCut(w, 30);
    const { rpcSeed } = await seedOf(w);
    await rpcSeed();
    await expectError(setCut(w, 0), "BasketImmutable");
    await expectError(propose(w, 60), "BasketImmutable");
    assert.equal((await fetch(w)).creatorCutBps, 30);
    await mint(w, w.alice);
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), "30000000");
  });
});

describe("creator cut: claiming it", () => {
  it("anyone can claim, only to the creator, and only what's owed", async () => {
    const w = await setupCut();
    await cutOn(w, 60);
    await mint(w, w.alice);
    const aliceShares = ata(w.shareMint, w.alice.publicKey, w.shareProgram);
    await expectError(claim(w, aliceShares), "ConstraintTokenOwner");

    const mine = ata(w.shareMint, w.creator.publicKey, w.shareProgram);
    const before = balance(w.ctx, mine);
    const treasury = fees(w).treasury;
    await claim(w);
    assert.equal(balance(w.ctx, mine) - before, 60_000_000n);
    assert.equal(fees(w).treasury, treasury - 60_000_000n);
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), "0");

    await claim(w);
    assert.equal(balance(w.ctx, mine) - before, 60_000_000n, "a second claim moves nothing");
  });

  it("the team can't spend what creators are owed", async () => {
    const w = await setupCut();
    await cutOn(w, 60);
    await mint(w, w.alice);
    const dest = await createAta(w.ctx, w.shareMint, w.ctx.payer.publicKey, w.shareProgram);
    const free = fees(w).treasury - 60_000_000n;
    await expectError(withdraw(w, free + 1n, dest), "TreasuryOwedToCreator");
    await withdraw(w, free, dest);
    assert.equal(balance(w.ctx, dest), free);
    await expectError(withdraw(w, 1n, dest), "TreasuryOwedToCreator");
    await claim(w);
    assert.equal(fees(w).treasury, 0n);
  });

  it("works on a Token-2022 share mint, and a split doesn't change what's owed", async () => {
    const w = await setupCut({ shareTokenProgram: TOKEN_2022_PROGRAM_ID });
    await cutOn(w, 60);
    await mint(w, w.alice);
    await rpc(w.ctx, w.ctx.vault.methods.split(2, 1).accountsPartial({ authority: w.creator.publicKey, basket: w.basket, shareMint: w.shareMint, tokenProgram: TOKEN_2022_PROGRAM_ID }), [w.creator]);
    const mine = ata(w.shareMint, w.creator.publicKey, w.shareProgram);
    const before = balance(w.ctx, mine);
    await claim(w);
    assert.equal(balance(w.ctx, mine) - before, 60_000_000n);
  });

  it("the shares claimed redeem like any others", async () => {
    const w = await setupCut();
    await cutOn(w, 60);
    await mint(w, w.alice);
    await claim(w);
    await rpc(w.ctx, redeemIx(w, w.creator, 60_000_000n), [w.creator]);
  });
});

describe("creator cut: the account itself", () => {
  it("keeps the basket's size, so no live basket needs moving", async () => {
    const w = await setupCut();
    const size = w.ctx.svm.getAccount(w.basket)!.data.length;
    await cutOn(w, 60);
    await mint(w, w.alice);
    assert.equal(w.ctx.svm.getAccount(w.basket)!.data.length, size);
  });
});

describe("creator cut: the notice as a param", () => {
  it("a rise waits the CUT_NOTICE_SECS in force when it's asked for; a later change moves no notice already running", async () => {
    const w = await setupCut();
    await mint(w, w.alice);
    // Two days, after its own week of notice.
    await setParam(w.ctx, PARAM.CUT_NOTICE_SECS, BigInt(2 * DAY));
    await propose(w, 20);
    const first = (await w.ctx.vault.account.cutNotice.fetch(pda.cutNotice(w.basket))).at.toNumber();
    assert.equal(first, now(w.ctx) + DAY, "the day still applies during the param's notice");
    warp(w.ctx, DAY + 1);
    await apply(w);
    warp(w.ctx, PARAM_NOTICE);
    await propose(w, 40);
    const at = (await w.ctx.vault.account.cutNotice.fetch(pda.cutNotice(w.basket))).at.toNumber();
    assert.equal(at, now(w.ctx) + 2 * DAY);
    // Back to a day: the rise already asked for still waits its two.
    await setParam(w.ctx, PARAM.CUT_NOTICE_SECS, BigInt(DAY));
    warp(w.ctx, PARAM_NOTICE);
    assert.equal((await w.ctx.vault.account.cutNotice.fetch(pda.cutNotice(w.basket))).at.toNumber(), at);
  });
});

/** The opening deposit of a basket built with `seed: false`. */
async function seedOf(w: World & { ctx: Ctx }) {
  const { SEED_SHARES, UNITS_A, UNITS_B, quads } = await import("./fixtures.ts");
  return {
    rpcSeed: () =>
      rpc(
        w.ctx,
        w.ctx.vault.methods
          .seedBasket(new BN(SEED_SHARES.toString()), [new BN(UNITS_A.toString()), new BN(UNITS_B.toString())])
          .accountsPartial({
            payer: w.creator.publicKey,
            basket: w.basket,
            shareMint: w.shareMint,
            payerShares: ata(w.shareMint, w.creator.publicKey, w.shareProgram),
            ...feeShares(w.shareMint, w.shareProgram),
            shareTokenProgram: w.shareProgram,
          })
          .remainingAccounts(quads(w, w.creator.publicKey)),
        [w.creator],
      ),
  };
}
