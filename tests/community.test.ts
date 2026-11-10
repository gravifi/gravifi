// Share staking and community baskets: any basket's holders can stake its shares, and in a community
// basket staked shares vote on what it holds. Weight builds over 3 days and is read as of a proposal's
// opening; a proposal takes a 3-day vote and a 24-hour wait, and every change stays within the limits
// a smart basket's strategist is held to.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { Keypair, PublicKey } from "@solana/web3.js";
import {
  BN,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  ata,
  balance,
  createAta,
  createMint,
  expectError,
  fund,
  mintTo,
  pda,
  postPrices,
  rpc,
  send,
  supply,
  warp,
  type Ctx,
} from "./helpers.ts";
import { NO_GRAVI, feeShares, fundHolder, listAsset, mintIx, redeemIx, setCreationBurn, world, type WorldOpts, initKeepers, setParamIx, CONFIG, setParam, PARAM, PARAM_NOTICE, FAMILY, potOf, potOwner, USDC } from "./fixtures.ts";
import { closeBasket, closeVaults, gone } from "./_close.ts";

const DAY = 86_400;
const SHARES = 1_000_000_000n;
const VOTE = 3 * DAY;
const WAIT = DAY;
const n = (b: { toString(): string }) => BigInt(b.toString());
const hash = (s: string) => Array.from(createHash("sha256").update(s).digest());
const GUARD = { maxDayBps: 2_000, maxWeekBps: 5_000, maxWeightBps: 3_500, maxUnverifiedBps: 3_000, minAssets: 2, maxAssets: 6, stopBps: 0, stopTrailing: false };

type Action = Record<string, any>;

async function communityWorld(opts: WorldOpts = {}) {
  const w = await world(opts);
  const { ctx } = w;
  warp(ctx, 1_000_000); // LiteSVM's clock starts at 0, which a real one never reads
  const alice = fund(ctx);
  const bob = fund(ctx);
  await fundHolder(w, alice);
  await fundHolder(w, bob);
  await rpc(ctx, mintIx(w, alice, 90n * SHARES), [alice]);
  await rpc(ctx, mintIx(w, bob, 900n * SHARES), [bob]);
  // Supply: 1,000 shares. The creator seeded 10.
  await createAta(ctx, w.shareMint, pda.shareEscrow(), w.shareProgram);
  return { ...w, alice, bob };
}
type W = Awaited<ReturnType<typeof communityWorld>>;

const openDoor = (w: Pick<W, "ctx">, open = true, authority = w.ctx.payer) =>
  rpc(w.ctx, setParamIx(w.ctx, CONFIG.communityOpen, open ? 1 : 0, [], authority.publicKey), [authority]);

const enter = (w: W, who = w.creator) =>
  rpc(w.ctx, w.ctx.vault.methods.enterCommunity().accountsPartial({ creator: who.publicKey, basket: w.basket, community: pda.community(w.basket), ext: pda.ext(w.basket), protocol: pda.protocol() }), [who]);

async function goCommunity(opts: WorldOpts = {}) {
  const w = await communityWorld(opts);
  await openDoor(w);
  await enter(w);
  return w;
}

const shareStake = (w: W, who: Keypair, amount: bigint) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.shareStake(new BN(amount.toString())).accountsPartial({
      wallet: who.publicKey,
      basket: w.basket,
      shareMint: w.shareMint,
      walletShares: ata(w.shareMint, who.publicKey, w.shareProgram),
      ext: pda.ext(w.basket),
      escrow: ata(w.shareMint, pda.shareEscrow(), w.shareProgram),
      shareStake: pda.shareStake(w.basket, who.publicKey),
      shareTokenProgram: w.shareProgram,
    }).remainingAccounts([{ pubkey: potOf(w.basket), isSigner: false, isWritable: false }]),
    [who],
  );

/** Lifts the day a stake stays (SHARE_STAKE_LOCK_SECS), for tests about something else: its
 *  `locked_until` back to 0. Not for a stake that voted. */
function unlock(w: W, who: Keypair) {
  const k = pda.shareStake(w.basket, who.publicKey);
  const a = w.ctx.svm.getAccount(k)!;
  const d = Buffer.from(a.data);
  d.writeBigInt64LE(0n, 8 + 1 + 32 + 32 + 8 + 8);
  w.ctx.svm.setAccount(k, { ...a, data: d });
}

const shareUnstake = (w: W, who: Keypair, amount: bigint) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.shareUnstake(new BN(amount.toString())).accountsPartial({
      wallet: who.publicKey,
      basket: w.basket,
      shareMint: w.shareMint,
      walletShares: ata(w.shareMint, who.publicKey, w.shareProgram),
      ext: pda.ext(w.basket),
      escrow: ata(w.shareMint, pda.shareEscrow(), w.shareProgram),
      shareStake: pda.shareStake(w.basket, who.publicKey),
      shareTokenProgram: w.shareProgram,
    }).remainingAccounts([{ pubkey: potOf(w.basket), isSigner: false, isWritable: false }]),
    [who],
  );

const community = (w: W) => w.ctx.vault.account.community.fetch(pda.community(w.basket)) as Promise<any>;
const basketOf = (w: W) => w.ctx.vault.account.basket.fetch(w.basket) as Promise<any>;
const proposalOf = (w: W, id: number) => w.ctx.vault.account.proposal.fetch(pda.proposal(w.basket, id)) as Promise<any>;

/** Opens a proposal; returns its id. */
async function propose(w: W, who: Keypair, action: Action, gravi: { burns?: boolean; stake?: Keypair } = {}) {
  const id = (await community(w)).nextId as number;
  await rpc(
    w.ctx,
    w.ctx.vault.methods
      .communityPropose(action)
      .accountsPartial({
        proposer: who.publicKey,
        basket: w.basket,
        shareMint: w.shareMint,
        shareStake: pda.shareStake(w.basket, who.publicKey),
        community: pda.community(w.basket),
        proposal: pda.proposal(w.basket, id),
        protocol: pda.protocol(),
        creationBurn: pda.creationBurn(),
        ...(gravi.burns
          ? { graviMint: w.graviMint, proposerGravi: ata(w.graviMint, who.publicKey, TOKEN_PROGRAM_ID), graviTokenProgram: TOKEN_PROGRAM_ID }
          : { graviMint: null, proposerGravi: null, graviTokenProgram: null }),
      })
      .remainingAccounts(
        gravi.stake
          ? [
              { pubkey: pda.stake(gravi.stake.publicKey), isSigner: false, isWritable: true },
              { pubkey: pda.stakePool(), isSigner: false, isWritable: false },
            ]
          : [],
      ),
    [who],
  );
  return id;
}

const vote = (w: W, who: Keypair, id: number, support = true) => {
  const proposal = pda.proposal(w.basket, id);
  return rpc(w.ctx, w.ctx.vault.methods.communityVote(support).accountsPartial({ wallet: who.publicKey, proposal, shareStake: pda.shareStake(w.basket, who.publicKey), vote: pda.vote(proposal, who.publicKey) }), [who]);
};

const execute = (w: W, id: number, verified: PublicKey[] = []) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods
      .communityExecute()
      .accountsPartial({ basket: w.basket, community: pda.community(w.basket), proposal: pda.proposal(w.basket, id) })
      // A Reweight names each asset's Verified record; a Leave, the extension account (the stakers' cut).
      .remainingAccounts(verified.length ? verified.map((pubkey) => ({ pubkey, isSigner: false, isWritable: false })) : [{ pubkey: pda.ext(w.basket), isSigner: false, isWritable: true }]),
  );

/** A wallet's one open proposal on the basket. */
const openOf = (w: W, who: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("open"), w.basket.toBuffer(), who.toBuffer()], w.ctx.vault.programId)[0];

const close = (w: W, id: number, proposer: PublicKey) =>
  rpc(w.ctx, w.ctx.vault.methods.communityClose().accountsPartial({ community: pda.community(w.basket), proposal: pda.proposal(w.basket, id), proposer, openProposal: openOf(w, proposer) }));

/** Bob has 90% staked and fully weighted, so what he proposes and backs passes. */
async function bobReady(w: W) {
  await shareStake(w, w.bob, 900n * SHARES);
  warp(w.ctx, VOTE);
}

/** Bob as in `bobReady`, and Alice (9%) and Carol (20 shares, about 2%) staked enough to propose:
 *  each wallet has one open proposal at a time. Returns Carol. */
async function crewReady(w: W) {
  const carol = fund(w.ctx);
  await fundHolder(w, carol);
  await rpc(w.ctx, mintIx(w, carol, 20n * SHARES), [carol]);
  await shareStake(w, carol, 20n * SHARES);
  await shareStake(w, w.alice, 90n * SHARES);
  await bobReady(w);
  return carol;
}

/** Proposes (Bob, unless `who`), passes with Bob's vote and waits out `action`, and returns its
 *  id, ready to execute. */
async function passed(w: W, action: Action, extra: { burns?: boolean; stake?: Keypair } = {}, who?: Keypair) {
  const id = await propose(w, who ?? w.bob, action, extra);
  await vote(w, w.bob, id);
  warp(w.ctx, VOTE + WAIT);
  return id;
}

const mintsHash = async (w: W) => {
  const c = (await basketOf(w)).constituents as { mint: PublicKey }[];
  return Array.from(createHash("sha256").update(Buffer.concat(c.map((x) => x.mint.toBuffer()))).digest());
};
const reweight = async (w: W, targetBps: number[]): Promise<Action> => ({ reweight: { targetBps, mintsHash: await mintsHash(w) } });
const targets = async (w: W) => ((await basketOf(w)).constituents as { targetBps: number }[]).map((c) => c.targetBps);
const verifiedOf = async (w: W) => ((await basketOf(w)).constituents as { mint: PublicKey }[]).map((c) => pda.verified(c.mint));

/** Lists C and readies the basket's vault for it. */
async function assetC(w: W) {
  const C = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
  await listAsset(w.ctx, C);
  const vault = await createAta(w.ctx, C, w.basket, TOKEN_PROGRAM_ID);
  return { C, vault };
}

const executeAdd = (w: W, id: number, mint: PublicKey, vault: PublicKey) =>
  rpc(w.ctx, w.ctx.vault.methods.communityExecuteAdd().accountsPartial({ basket: w.basket, community: pda.community(w.basket), proposal: pda.proposal(w.basket, id), mint, assetEntry: pda.asset(mint), vault }));

describe("share staking", () => {
  it("works on any basket, in and out, and closes when empty", async () => {
    const w = await communityWorld();
    const shares = ata(w.shareMint, w.alice.publicKey);
    await shareStake(w, w.alice, 40n * SHARES);
    assert.equal(balance(w.ctx, shares), 50n * SHARES);
    assert.equal(balance(w.ctx, ata(w.shareMint, pda.shareEscrow())), 40n * SHARES);
    // A stake stays a day: nobody stakes for a moment just to take a cut.
    await expectError(shareUnstake(w, w.alice, 10n * SHARES), "SharesLocked");
    warp(w.ctx, 86_400);
    await shareUnstake(w, w.alice, 10n * SHARES);
    assert.equal(balance(w.ctx, shares), 60n * SHARES);
    await expectError(shareUnstake(w, w.alice, 31n * SHARES), "NotEnoughStaked");
    await shareUnstake(w, w.alice, 30n * SHARES);
    assert.equal(w.ctx.svm.getAccount(pda.shareStake(w.basket, w.alice.publicKey))?.lamports ?? 0, 0);
  });

  it("someone else's stake can't be taken", async () => {
    const w = await communityWorld();
    await shareStake(w, w.alice, 40n * SHARES);
    await expectError(
      rpc(
        w.ctx,
        w.ctx.vault.methods.shareUnstake(new BN(1)).accountsPartial({
          wallet: w.bob.publicKey,
          basket: w.basket,
          shareMint: w.shareMint,
          walletShares: ata(w.shareMint, w.bob.publicKey),
          ext: pda.ext(w.basket),
          escrow: ata(w.shareMint, pda.shareEscrow()),
          shareStake: pda.shareStake(w.basket, w.alice.publicKey),
          shareTokenProgram: w.shareProgram,
        }),
        [w.bob],
      ),
      "ConstraintSeeds",
    );
  });

  it("works with Token-2022 shares, whose splits don't touch what's staked", async () => {
    const w = await communityWorld({ shareTokenProgram: TOKEN_2022_PROGRAM_ID });
    await shareStake(w, w.alice, 40n * SHARES);
    assert.equal(balance(w.ctx, ata(w.shareMint, pda.shareEscrow(), TOKEN_2022_PROGRAM_ID)), 40n * SHARES);
    unlock(w, w.alice);
    await shareUnstake(w, w.alice, 40n * SHARES);
  });
});

describe("community: entering", () => {
  it("only while the door's open, only its creator, only seeded, public and creator-managed", async () => {
    const w = await communityWorld();
    await expectError(enter(w), "CommunityClosed");
    await expectError(openDoor(w, true, fund(w.ctx)), "NotProtocolAuthority");
    await openDoor(w);
    await expectError(enter(w, w.alice), "NotMandateAuthority");
    await enter(w);
    await expectError(enter(w), "CommunityNotAllowed");
    assert.ok((await basketOf(w)).control.community);

    for (const opts of [{ access: "allowlist" }, { control: "immutable" }, { seed: false }] as WorldOpts[]) {
      const other = (await world(opts)) as unknown as W;
      await openDoor(other);
      await expectError(enter(other), "CommunityNotAllowed");
    }
  });

  it("no basket is created in Community mode", async () => {
    const w = await communityWorld();
    const basketId = new Uint8Array(16).fill(3);
    const basket = pda.basket(w.creator.publicKey, basketId);
    await expectError(
      rpc(
        w.ctx,
        w.ctx.vault.methods
          .initializeBasket({ basketId: Array.from(basketId), access: { public: {} }, control: { community: {} }, rebalanceMode: { hybrid: {} }, intervalSecs: new BN(86_400), driftBps: 500, mgmtFeeBps: 20, mints: [w.mintA, w.mintB], targetBps: [5_000, 5_000] })
          .accountsPartial({ creator: w.creator.publicKey, basket, shareMint: pda.share(basket), shareTokenProgram: TOKEN_PROGRAM_ID, ...NO_GRAVI })
          .remainingAccounts([w.mintA, pda.asset(w.mintA), w.mintB, pda.asset(w.mintB)].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false }))),
        [w.creator],
      ),
      "CommunityNotAllowed",
    );
  });

  it("the creator loses every power over what it holds, and closing the door leaves it running", async () => {
    const w = await goCommunity();
    const c = w.creator;
    const reconfigure = { authority: c.publicKey, basket: w.basket };
    await expectError(rpc(w.ctx, w.ctx.vault.methods.setTargets([5_000, 5_000]).accountsPartial(reconfigure), [c]), "CommunityManaged");
    await expectError(rpc(w.ctx, w.ctx.vault.methods.setPolicy({ hybrid: {} }, new BN(86_400), 500, 0).accountsPartial(reconfigure), [c]), "CommunityManaged");
    await expectError(rpc(w.ctx, w.ctx.vault.methods.removeConstituent(0).accountsPartial(reconfigure), [c]), "CommunityManaged");
    await expectError(rpc(w.ctx, w.ctx.vault.methods.setStops([]).accountsPartial({ creator: c.publicKey, basket: w.basket, stopLoss: pda.stop(w.basket) }), [c]), "CommunityManaged");
    await openDoor(w, false);
    await bobReady(w);
    await propose(w, w.bob, { leave: {} });
  });
});

describe("community: weight", () => {
  it("proposing takes 1% of the supply, built over 3 days", async () => {
    const w = await goCommunity();
    await shareStake(w, w.alice, 10n * SHARES); // 1% of 1,000, once fully built
    await expectError(propose(w, w.alice, { leave: {} }), "BelowProposalWeight");
    warp(w.ctx, VOTE - 1);
    await expectError(propose(w, w.alice, { leave: {} }), "BelowProposalWeight");
    warp(w.ctx, 1);
    await propose(w, w.alice, { leave: {} });
  });

  it("flash-minted shares carry nothing: weight is read as of the opening", async () => {
    const w = await goCommunity();
    await bobReady(w);
    const id = await propose(w, w.bob, { creatorFee: { mgmtFeeBps: 0 } });
    await shareStake(w, w.alice, 90n * SHARES);
    await expectError(vote(w, w.alice, id, false), "NoVotingWeight");
    warp(w.ctx, VOTE - 1); // it has built by now, but not before the opening
    await expectError(vote(w, w.alice, id, false), "NoVotingWeight");
  });

  it("a top-up keeps today's weight: old age doesn't lend itself to new shares", async () => {
    const w = await goCommunity();
    await shareStake(w, w.bob, 100n * SHARES);
    warp(w.ctx, 300 * DAY);
    await shareStake(w, w.bob, 800n * SHARES);
    const id = await propose(w, w.bob, { leave: {} }).catch(() => -1);
    assert.equal(id, 0, "100 of 1,000 still clears the 1% to propose");
    await vote(w, w.bob, id);
    const p = await proposalOf(w, id);
    assert.ok(n(p.votesFor) < 101n * SHARES, `weight ${p.votesFor} is about the 100 that had built`);
    assert.ok(n(p.votesFor) >= 99n * SHARES);
  });

  it("shares vote once, and stay staked until the vote ends", async () => {
    const w = await goCommunity();
    await bobReady(w);
    const id = await propose(w, w.bob, { leave: {} });
    await vote(w, w.bob, id);
    await expectError(vote(w, w.bob, id), "already in use");
    await expectError(shareUnstake(w, w.bob, SHARES), "SharesLocked");
    warp(w.ctx, VOTE);
    await shareUnstake(w, w.bob, SHARES); // in time to leave during the wait
    await expectError(vote(w, w.alice, id), "AccountNotInitialized");
  });

  it("moving voted shares to a new wallet doesn't vote them again", async () => {
    const w = await goCommunity();
    await bobReady(w);
    await shareStake(w, w.alice, 90n * SHARES);
    const id = await propose(w, w.bob, { creatorFee: { mgmtFeeBps: 0 } });
    await expectError(vote(w, w.alice, id), "NoVotingWeight");
    await expectError(vote(w, w.alice, id, false), "NoVotingWeight");
  });
});

describe("community: passing", () => {
  it("waits out the vote and 24 hours, and lapses after 7 days", async () => {
    const w = await goCommunity();
    await shareStake(w, w.alice, 10n * SHARES);
    await bobReady(w);
    const id = await propose(w, w.bob, { policy: { rebalanceMode: { periodic: {} }, intervalSecs: new BN(2 * DAY), driftBps: 600 } });
    await vote(w, w.bob, id);
    await expectError(execute(w, id), "NotExecutable");
    warp(w.ctx, VOTE);
    await expectError(vote(w, w.alice, id), "VotingOver");
    await expectError(execute(w, id), "NotExecutable");
    warp(w.ctx, WAIT - 1);
    await expectError(execute(w, id), "NotExecutable");
    warp(w.ctx, 1 + 7 * DAY + 1);
    await expectError(execute(w, id), "NotExecutable");
    await close(w, id, w.bob.publicKey);
  });

  it("anyone runs a passed proposal, once", async () => {
    const w = await goCommunity();
    await bobReady(w);
    const id = await passed(w, { policy: { rebalanceMode: { periodic: {} }, intervalSecs: new BN(2 * DAY), driftBps: 600 } });
    await execute(w, id);
    const b = await basketOf(w);
    assert.ok(b.rebalanceMode.periodic);
    assert.equal(b.driftBps, 600);
    await expectError(execute(w, id), "NotExecutable");
  });

  it("a proposal's windows are fixed when it opens: a later change to the vote's length moves none already open", async () => {
    const w = await goCommunity();
    const carol = await crewReady(w);
    await setParam(w.ctx, PARAM.VOTE_PERIOD_SECS, BigInt(DAY));
    warp(w.ctx, PARAM_NOTICE - 3_600);
    const before = await propose(w, w.bob, { policy: { rebalanceMode: { periodic: {} }, intervalSecs: new BN(2 * DAY), driftBps: 600 } });
    warp(w.ctx, 7_200);
    const after = await propose(w, w.alice, { policy: { rebalanceMode: { periodic: {} }, intervalSecs: new BN(3 * DAY), driftBps: 700 } });
    // Each carries its terms after its action.
    const len = (id: number) => w.ctx.svm.getAccount(pda.proposal(w.basket, id))!.data.length;
    assert.equal(len(before), len(after));
    warp(w.ctx, DAY + 1);
    await expectError(vote(w, carol, after), "VotingOver");
    await vote(w, carol, before);
    // A proposal opened before the terms were written (sized for its action alone) reads the
    // constants: still three days, whatever the param says.
    const acct = w.ctx.svm.getAccount(pda.proposal(w.basket, before))!;
    w.ctx.svm.setAccount(pda.proposal(w.basket, before), { ...acct, data: acct.data.slice(0, acct.data.length - 29) });
    warp(w.ctx, DAY);
    await vote(w, w.bob, before);
    warp(w.ctx, DAY);
    await expectError(vote(w, w.alice, before), "VotingOver");
  });

  it("the community switch stops entering, proposing and running a vote, but never leaving", async () => {
    const w = await goCommunity();
    await bobReady(w);
    const policy = await passed(w, { policy: { rebalanceMode: { periodic: {} }, intervalSecs: new BN(2 * DAY), driftBps: 600 } });
    await close(w, policy, w.bob.publicKey).catch(() => {});
    const carol = fund(w.ctx);
    await fundHolder(w, carol);
    await rpc(w.ctx, mintIx(w, carol, 20n * SHARES), [carol]);
    await shareStake(w, carol, 20n * SHARES);
    warp(w.ctx, VOTE);
    const leave = await propose(w, carol, { leave: {} });
    await vote(w, w.bob, leave);
    warp(w.ctx, VOTE + WAIT);
    await setParam(w.ctx, PARAM.KILL, BigInt(FAMILY.community));
    await expectError(execute(w, policy), "FeatureOff");
    await expectError(propose(w, w.bob, { leave: {} }), "FeatureOff");
    await execute(w, leave);
    assert.ok((await basketOf(w)).control.creator);
    const other = await communityWorld();
    await openDoor(other);
    await setParam(other.ctx, PARAM.KILL, BigInt(FAMILY.community));
    await expectError(enter(other), "FeatureOff");
    await setParam(other.ctx, PARAM.KILL, 0n);
    await enter(other);
  });

  it("needs 20% of the supply to vote", async () => {
    const w = await goCommunity();
    await shareStake(w, w.bob, 190n * SHARES); // 19%
    warp(w.ctx, VOTE);
    const id = await propose(w, w.bob, { policy: { rebalanceMode: { hybrid: {} }, intervalSecs: new BN(2 * DAY), driftBps: 600 } });
    await vote(w, w.bob, id);
    warp(w.ctx, VOTE + WAIT);
    await expectError(execute(w, id), "NotExecutable");
    await close(w, id, w.bob.publicKey);
  });

  it("needs over half, and 66% for the fee, going Smart and leaving", async () => {
    const w = await communityWorld();
    await openDoor(w);
    await rpc(w.ctx, w.ctx.vault.methods.setPolicy({ hybrid: {} }, new BN(30 * DAY), 500, 20).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }), [w.creator]);
    await enter(w);
    // A vote split 60/40.
    const carol = fund(w.ctx);
    await fundHolder(w, carol);
    await rpc(w.ctx, mintIx(w, carol, 600n * SHARES), [carol]);
    await shareStake(w, w.bob, 900n * SHARES);
    await shareStake(w, carol, 600n * SHARES);
    warp(w.ctx, VOTE);
    const fee = await propose(w, w.bob, { creatorFee: { mgmtFeeBps: 0 } });
    const policy = await propose(w, carol, { policy: { rebalanceMode: { hybrid: {} }, intervalSecs: new BN(2 * DAY), driftBps: 600 } });
    for (const id of [fee, policy]) {
      await vote(w, w.bob, id);
      await vote(w, carol, id, false);
    }
    warp(w.ctx, VOTE + WAIT);
    await expectError(execute(w, fee), "NotExecutable");
    await execute(w, policy);
  });

  it("one open proposal per wallet, so filling every slot takes three of them (M3)", async () => {
    const w = await goCommunity();
    const carol = await crewReady(w);
    const action = { split: { numerator: 2, denominator: 1 } };
    const rent = w.ctx.svm.getBalance(w.alice.publicKey)!;
    const a = await propose(w, w.alice, action);
    assert.ok(w.ctx.svm.getAccount(openOf(w, w.alice.publicKey)), "Alice's open proposal, on her rent");
    // One wallet with 1% staked can't hold every slot by itself.
    await expectError(propose(w, w.alice, action), "ProposerHasOpenProposal");
    await propose(w, w.bob, action);
    await expectError(propose(w, w.bob, action), "ProposerHasOpenProposal");
    await propose(w, carol, action);
    // Closed (failed: nobody voted), she can propose again, and her rent comes back.
    warp(w.ctx, VOTE);
    await close(w, a, w.alice.publicKey);
    assert.equal(w.ctx.svm.getAccount(openOf(w, w.alice.publicKey))?.lamports ?? 0, 0);
    assert.equal(w.ctx.svm.getBalance(w.alice.publicKey)!, rent);
    await propose(w, w.alice, action);
  });

  it("at most 3 open at once; a done one closes, its rent back, and its votes too", async () => {
    const w = await goCommunity();
    const carol = await crewReady(w);
    const dave = fund(w.ctx);
    await fundHolder(w, dave);
    await rpc(w.ctx, mintIx(w, dave, 20n * SHARES), [dave]);
    await shareStake(w, dave, 20n * SHARES);
    warp(w.ctx, VOTE);
    const action = { split: { numerator: 2, denominator: 1 } };
    const a = await propose(w, w.bob, action);
    await propose(w, w.alice, action);
    await propose(w, carol, action);
    await expectError(propose(w, dave, action), "TooManyProposals");
    await vote(w, w.bob, a);
    await expectError(close(w, a, w.bob.publicKey), "ProposalStillOpen");
    const closeVote = () => rpc(w.ctx, w.ctx.vault.methods.communityCloseVote().accountsPartial({ vote: pda.vote(pda.proposal(w.basket, a), w.bob.publicKey), wallet: w.bob.publicKey }));
    await expectError(closeVote(), "ProposalStillOpen");
    warp(w.ctx, VOTE);
    await expectError(close(w, a, w.bob.publicKey), "ProposalStillOpen"); // passed: it may yet run
    await closeVote();
    await close(w, 1, w.alice.publicKey); // no votes: failed
    assert.equal((await community(w)).open, 2);
    await propose(w, dave, action);
    assert.equal((await community(w)).nextId, 4, "ids never repeat");
  });
});

describe("community: what a vote does", () => {
  it("adds a listed asset, the proposer burning for it win or lose, unless their stake qualifies", async () => {
    const w = await goCommunity({});
    await setCreationBurn(w.ctx, w.graviMint, 10_000n * 1_000_000_000n);
    await bobReady(w);
    const { C, vault } = await assetC(w);
    await expectError(propose(w, w.bob, { addAsset: { mint: C } }), "CreationBurnAccountsMissing");
    const gravi = await createAta(w.ctx, w.graviMint, w.bob.publicKey, TOKEN_PROGRAM_ID);
    await mintTo(w.ctx, w.graviMint, gravi, 50_000n * 1_000_000_000n);
    const sup = supply(w.ctx, w.graviMint);
    const id = await passed(w, { addAsset: { mint: C } }, { burns: true });
    assert.equal(supply(w.ctx, w.graviMint), sup - 10_000n * 1_000_000_000n);
    const other = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
    await expectError(executeAdd(w, id, other, vault), "ConstituentMismatch");
    await executeAdd(w, id, C, vault);
    const b = await basketOf(w);
    assert.equal(b.constituentCount, 3);
    assert.equal(b.constituents[2].targetBps, 0);
  });

  it("reweights within the weight caps, Verified or not", async () => {
    const w = await goCommunity();
    await crewReady(w);
    const { C, vault } = await assetC(w);
    const add = await passed(w, { addAsset: { mint: C } });
    await executeAdd(w, add, C, vault);
    await close(w, add, w.bob.publicKey);

    // A is 60%, B 40%. C unverified can't take 31%.
    const over = await passed(w, await reweight(w, [4_000, 2_900, 3_100]));
    await expectError(execute(w, over, await verifiedOf(w)), "WeightAboveCap");
    const ok = await passed(w, await reweight(w, [4_000, 3_000, 3_000]), {}, w.alice);
    await execute(w, ok, await verifiedOf(w));
    assert.deepEqual(await targets(w), [4_000, 3_000, 3_000]);
    await close(w, ok, w.alice.publicKey);
    // At least two assets stay above 0%.
    await expectError(propose(w, w.alice, await reweight(w, [10_000, 0, 0])), "AssetCountOutOfRange");
  });

  it("stays within the change limits, day and week", async () => {
    const w = await goCommunity();
    const carol = await crewReady(w);
    for (let i = 0; i < 4; i++) {
      const { C, vault } = await assetC(w);
      await rpc(w.ctx, w.ctx.vault.methods.setVerified(true).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol(), mint: C, verified: pda.verified(C) }));
      const id = await passed(w, { addAsset: { mint: C } });
      await executeAdd(w, id, C, vault);
      await close(w, id, w.bob.publicKey);
    }
    await rpc(w.ctx, w.ctx.vault.methods.setVerified(true).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol(), mint: w.mintA, verified: pda.verified(w.mintA) }));
    await rpc(w.ctx, w.ctx.vault.methods.setVerified(true).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol(), mint: w.mintB, verified: pda.verified(w.mintB) }));
    // A is 60%, B 40%, the other four 0%. Over the day's 40% in one go: refused on proposing.
    await expectError(propose(w, w.bob, await reweight(w, [2_000, 2_000, 2_000, 2_000, 2_000, 0])), "TurnoverDayCap");

    // Each is 40% from where the basket stands now, so all three can be proposed together. Run in
    // order they move 40%, 20% and 40%.
    const proposers = [w.bob, w.alice, carol];
    const ids = [
      await propose(w, w.bob, await reweight(w, [3_000, 3_000, 1_000, 1_000, 1_000, 1_000])),
      await propose(w, w.alice, await reweight(w, [3_000, 3_000, 0, 0, 2_000, 2_000])),
      await propose(w, carol, await reweight(w, [3_000, 3_000, 2_000, 2_000, 0, 0])),
    ];
    for (const id of ids) await vote(w, w.bob, id);
    warp(w.ctx, VOTE + WAIT);
    await execute(w, ids[0], await verifiedOf(w));
    await expectError(execute(w, ids[1], await verifiedOf(w)), "TurnoverDayCap");
    warp(w.ctx, DAY);
    await execute(w, ids[1], await verifiedOf(w));
    await expectError(execute(w, ids[2], await verifiedOf(w)), "TurnoverDayCap");
    warp(w.ctx, DAY);
    await execute(w, ids[2], await verifiedOf(w));
    for (const [i, id] of ids.entries()) await close(w, id, proposers[i].publicKey);
    // The whole week's 100% is spent, 2 days in: a fourth has to wait for the first to leave the window.
    const fourth = await passed(w, await reweight(w, [3_000, 3_000, 1_000, 1_000, 1_000, 1_000]));
    await expectError(execute(w, fourth, await verifiedOf(w)), "TurnoverWeekCap");
    warp(w.ctx, DAY + 3_600);
    await execute(w, fourth, await verifiedOf(w));
    assert.deepEqual(await targets(w), [3_000, 3_000, 1_000, 1_000, 1_000, 1_000]);
  });

  it("targets pinned to assets that have since changed don't run", async () => {
    const w = await goCommunity();
    await crewReady(w);
    const { C, vault } = await assetC(w);
    const old = await propose(w, w.bob, await reweight(w, [5_000, 5_000]));
    await vote(w, w.bob, old);
    const add = await propose(w, w.alice, { addAsset: { mint: C } });
    await vote(w, w.bob, add);
    warp(w.ctx, VOTE + WAIT);
    await executeAdd(w, add, C, vault);
    await expectError(execute(w, old, await verifiedOf(w)), "AssetsChanged");
  });

  it("the creator fee only goes down", async () => {
    const w = await goCommunity();
    await bobReady(w);
    await expectError(propose(w, w.bob, { creatorFee: { mgmtFeeBps: 20 } }), "BadProposal");
    await expectError(propose(w, w.bob, { creatorFee: { mgmtFeeBps: 21 } }), "BadProposal");
    const id = await passed(w, { creatorFee: { mgmtFeeBps: 5 } });
    // Run alone it would forfeit what the old fee streamed since the last accrual (L4).
    await expectError(execute(w, id), "FeeNotAccrued");
    const creatorShares = ata(w.shareMint, w.creator.publicKey, w.shareProgram);
    const before = balance(w.ctx, creatorShares);
    const accrue = await w.ctx.vault.methods
      .accrueFees()
      .accountsPartial({ basket: w.basket, shareMint: w.shareMint, creatorShares, shareTokenProgram: w.shareProgram })
      .remainingAccounts([w.vaultA, w.vaultB].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false })))
      .instruction();
    const run = await w.ctx.vault.methods
      .communityExecute()
      .accountsPartial({ basket: w.basket, community: pda.community(w.basket), proposal: pda.proposal(w.basket, id) })
      .instruction();
    await send(w.ctx, [accrue, run]);
    assert.ok(balance(w.ctx, creatorShares) > before, "the old fee was paid up to the change");
    assert.equal((await basketOf(w)).mgmtFeeBps, 5);
  });

  it("sets stops, and a stop fires with no vote, within the Reweight caps (M1)", async () => {
    // Four assets at 25%, set by the creator before handing it over; B, C and D Verified.
    const w = await communityWorld();
    const extra: PublicKey[] = [];
    for (let i = 0; i < 2; i++) {
      const { C, vault } = await assetC(w);
      await rpc(w.ctx, w.ctx.vault.methods.addConstituent().accountsPartial({ ...NO_GRAVI, authority: w.creator.publicKey, basket: w.basket, mint: C, assetEntry: pda.asset(C), vault }), [w.creator]);
      extra.push(C);
    }
    await rpc(w.ctx, w.ctx.vault.methods.setTargets([2_500, 2_500, 2_500, 2_500]).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }), [w.creator]);
    for (const m of [w.mintB, ...extra]) await rpc(w.ctx, w.ctx.vault.methods.setVerified(true).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol(), mint: m, verified: pda.verified(m) }));
    await openDoor(w);
    await enter(w);
    await bobReady(w);
    const keeper = fund(w.ctx);
    await initKeepers(w.ctx, keeper.publicKey, keeper.publicKey);
    // Proposed at a reference of $1.20 (A's price when it opened, say); A is at $1 when it runs.
    const stops = [{ mint: w.mintA, pctBps: 1_000, trailing: false, referenceE12: new BN("1200000000000") }];
    const id = await passed(w, { stops: { stops } });
    await postPrices(w.ctx, keeper, [[w.mintA, 1_000_000_000_000n]]);
    const run = (prices: PublicKey[]) =>
      rpc(
        w.ctx,
        w.ctx.vault.methods
          .communityExecuteStops()
          .accountsPartial({ payer: w.ctx.payer.publicKey, basket: w.basket, community: pda.community(w.basket), proposal: pda.proposal(w.basket, id), stopLoss: pda.stop(w.basket) })
          .remainingAccounts(prices.map((pubkey) => ({ pubkey, isSigner: false, isWritable: false }))),
      );
    await expectError(run([]), "BadRemainingAccounts");
    await run([pda.price(w.mintA)]);
    // Lowered to the price: it doesn't start out armed (at $1.20 its level, $1.08, is above $1).
    const st = (await w.ctx.vault.account.stopLoss.fetch(pda.stop(w.basket))) as any;
    assert.equal(st.stops[0].referenceE12.toString(), "1000000000000");

    const records = (await verifiedOf(w)).map((pubkey) => ({ pubkey, isSigner: false, isWritable: false }));
    const check = (named = records) => rpc(w.ctx, w.ctx.vault.methods.checkStop().accountsPartial({ basket: w.basket, stopLoss: pda.stop(w.basket), assetPrice: pda.price(w.mintA) }).remainingAccounts(named));
    await check();
    assert.equal(((await w.ctx.vault.account.stopLoss.fetch(pda.stop(w.basket))) as any).stops[0].armedAt.toString(), "0");
    await postPrices(w.ctx, keeper, [[w.mintA, 800_000_000_000n]]);
    await check();
    warp(w.ctx, 300);
    await postPrices(w.ctx, keeper, [[w.mintA, 800_000_000_000n]]);
    await expectError(check([]), "BadRemainingAccounts");
    await check();
    // 25% each to 33.3%, under the 35% Verified cap.
    assert.deepEqual(await targets(w), [0, 3_334, 3_333, 3_333]);
  });

  it("a vote can't put a stop on a two-asset basket: two assets must stay without one", async () => {
    const w = await goCommunity();
    await bobReady(w);
    const keeper = fund(w.ctx);
    await initKeepers(w.ctx, keeper.publicKey, keeper.publicKey);
    await postPrices(w.ctx, keeper, [[w.mintA, 1_000_000_000_000n]]);
    await expectError(passed(w, { stops: { stops: [{ mint: w.mintA, pctBps: 1_000, trailing: false, referenceE12: new BN("1000000000000") }] } }), "TooManyStops");
  });

  it("splits", async () => {
    const w = await goCommunity({ shareTokenProgram: TOKEN_2022_PROGRAM_ID });
    await bobReady(w);
    const id = await passed(w, { split: { numerator: 2, denominator: 1 } });
    await rpc(w.ctx, w.ctx.vault.methods.communityExecuteSplit().accountsPartial({ basket: w.basket, community: pda.community(w.basket), proposal: pda.proposal(w.basket, id), shareMint: w.shareMint, tokenProgram: TOKEN_2022_PROGRAM_ID }));
    await expectError(rpc(w.ctx, w.ctx.vault.methods.split(2, 1).accountsPartial({ authority: w.creator.publicKey, basket: w.basket, shareMint: w.shareMint, tokenProgram: TOKEN_2022_PROGRAM_ID }), [w.creator]), "CommunityManaged");
  });

  it("removes a sold-out asset with no vote, for anyone, but not one a vote just added (M2)", async () => {
    const w = await goCommunity();
    await bobReady(w);
    const { C, vault } = await assetC(w);
    const add = await passed(w, { addAsset: { mint: C } });
    await executeAdd(w, add, C, vault);
    const remove = (i: number) => rpc(w.ctx, w.ctx.vault.methods.communityRemove(i).accountsPartial({ basket: w.basket, community: pda.community(w.basket) }));
    // C sits at 0% with nothing in it until a Reweight weights it: nobody may remove it meanwhile.
    await expectError(remove(2), "RemovalWaitsForVotes");
    await close(w, add, w.bob.publicKey);
    await expectError(remove(2), "AssetJustAdded");
    // A Reweight in the works pins the assets: nothing goes while it's open.
    warp(w.ctx, VOTE + WAIT + 7 * DAY);
    const rw = await propose(w, w.bob, await reweight(w, [5_000, 4_000, 1_000]));
    await expectError(remove(2), "RemovalWaitsForVotes");
    // Naming it doesn't help: a Reweight would be voided.
    const named = (i: number, ids: number[]) =>
      rpc(w.ctx, w.ctx.vault.methods.communityRemove(i).accountsPartial({ basket: w.basket, community: pda.community(w.basket) }).remainingAccounts(ids.map((id) => ({ pubkey: pda.proposal(w.basket, id), isSigner: false, isWritable: false }))));
    await expectError(named(2, [rw]), "RemovalWaitsForVotes");
    warp(w.ctx, VOTE);
    await close(w, rw, w.bob.publicKey); // nobody voted: failed
    // A vote that doesn't touch it (a policy change) no longer holds it up, once it's named:
    // three wallets can't keep an emptied asset in by opening proposals.
    const policy = await propose(w, w.bob, { policy: { rebalanceMode: { hybrid: {} }, intervalSecs: new BN(2 * DAY), driftBps: 600 } });
    await expectError(remove(2), "RemovalWaitsForVotes");
    await expectError(named(0, [policy]), "ConstituentNotEmpty");
    await named(2, [policy]);
    assert.equal((await basketOf(w)).constituentCount, 2);
    await expectError(named(0, [policy]), "TooFewConstituents");
  });

  it("removing with every open vote named, the old way, still works when none is open", async () => {
    const w = await goCommunity();
    await bobReady(w);
    const { C, vault } = await assetC(w);
    const add = await passed(w, { addAsset: { mint: C } });
    await executeAdd(w, add, C, vault);
    await close(w, add, w.bob.publicKey);
    warp(w.ctx, VOTE + WAIT + 7 * DAY);
    const remove = (i: number) => rpc(w.ctx, w.ctx.vault.methods.communityRemove(i).accountsPartial({ basket: w.basket, community: pda.community(w.basket) }));
    await expectError(remove(0), "ConstituentNotEmpty");
    await remove(2);
    assert.equal((await basketOf(w)).constituentCount, 2);
    await expectError(remove(0), "TooFewConstituents");
  });

  it("leaving hands it back and voids what was passed before", async () => {
    const w = await goCommunity();
    await crewReady(w);
    const policy = await propose(w, w.alice, { policy: { rebalanceMode: { hybrid: {} }, intervalSecs: new BN(2 * DAY), driftBps: 600 } });
    await vote(w, w.bob, policy);
    const id = await passed(w, { leave: {} });
    await execute(w, id);
    const b = await basketOf(w);
    assert.ok(b.control.creator);
    assert.equal(b.community, 0);
    await rpc(w.ctx, w.ctx.vault.methods.setTargets([5_000, 5_000]).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }), [w.creator]);

    await enter(w);
    await expectError(execute(w, policy), "NotExecutable");
    await close(w, policy, w.alice.publicKey);
  });
});

describe("community: the creator's cut", () => {
  const setCut = (w: W, bps: number, signer = w.creator) =>
    rpc(w.ctx, w.ctx.vault.methods.setCreatorCut(bps).accountsPartial({ authority: signer.publicKey, basket: w.basket, notice: null }), [signer]);
  const cutOf = async (w: W) => (await basketOf(w)).creatorCutBps as number;

  it("a vote can lower it, with 66%, and the creator can't touch it", async () => {
    const w = await communityWorld();
    await openDoor(w);
    await rpc(w.ctx, w.ctx.vault.methods.proposeCutRise(30).accountsPartial({ authority: w.creator.publicKey, basket: w.basket, notice: pda.cutNotice(w.basket) }), [w.creator]);
    warp(w.ctx, 2 * DAY);
    await rpc(w.ctx, w.ctx.vault.methods.applyCutRise().accountsPartial({ basket: w.basket, notice: pda.cutNotice(w.basket), creator: w.creator.publicKey }));
    assert.equal(await cutOf(w), 30);
    await enter(w);
    await expectError(setCut(w, 10), "CommunityManaged");
    await bobReady(w);
    await expectError(propose(w, w.bob, { creatorCut: { bps: 30 } }), "BadProposal");
    const id = await passed(w, { creatorCut: { bps: 10 } });
    await execute(w, id);
    assert.equal(await cutOf(w), 10);
  });

  it("a vote that lowers it calls off a rise asked for before, even after the basket leaves (bug bounty, 7 Nov)", async () => {
    const w = await communityWorld();
    await openDoor(w);
    const applyRise = () => rpc(w.ctx, w.ctx.vault.methods.applyCutRise().accountsPartial({ basket: w.basket, notice: pda.cutNotice(w.basket), creator: w.creator.publicKey }));
    await rpc(w.ctx, w.ctx.vault.methods.proposeCutRise(30).accountsPartial({ authority: w.creator.publicKey, basket: w.basket, notice: pda.cutNotice(w.basket) }), [w.creator]);
    warp(w.ctx, 2 * DAY);
    await applyRise();
    await rpc(w.ctx, w.ctx.vault.methods.proposeCutRise(60).accountsPartial({ authority: w.creator.publicKey, basket: w.basket, notice: pda.cutNotice(w.basket) }), [w.creator]);
    await enter(w);
    await bobReady(w);
    const lower = await passed(w, { creatorCut: { bps: 10 } });
    await execute(w, lower);
    assert.equal(await cutOf(w), 10);
    await close(w, lower, w.bob.publicKey);
    await execute(w, await passed(w, { leave: {} }));
    assert.ok((await basketOf(w)).control.creator);
    await expectError(applyRise(), "CutRiseVoid");
    assert.equal(await cutOf(w), 10);
  });

  it("a rise asked for before handing it over doesn't apply once holders run it", async () => {
    const w = await communityWorld();
    await openDoor(w);
    await rpc(w.ctx, w.ctx.vault.methods.proposeCutRise(60).accountsPartial({ authority: w.creator.publicKey, basket: w.basket, notice: pda.cutNotice(w.basket) }), [w.creator]);
    await enter(w);
    warp(w.ctx, 2 * DAY);
    await expectError(rpc(w.ctx, w.ctx.vault.methods.applyCutRise().accountsPartial({ basket: w.basket, notice: pda.cutNotice(w.basket), creator: w.creator.publicKey })), "CommunityManaged");
    assert.equal(await cutOf(w), 0);
  });
});

describe("community: handing it to the strategist", () => {
  const executeSmart = (w: W, id: number) =>
    rpc(w.ctx, w.ctx.vault.methods.communityExecuteSmart().accountsPartial({ payer: w.ctx.payer.publicKey, basket: w.basket, community: pda.community(w.basket), proposal: pda.proposal(w.basket, id), smart: pda.smart(w.basket), creator: w.creator.publicKey }).remainingAccounts([{ pubkey: pda.ext(w.basket), isSigner: false, isWritable: true }]));

  it("the vote holds the creator's Smart powers, and takes it back", async () => {
    const w = await goCommunity();
    await bobReady(w);
    const mandate = hash('{"goal":"momentum"}');
    const id = await passed(w, { goSmart: { mandateHash: mandate, guard: GUARD } });
    await executeSmart(w, id);
    await close(w, id, w.bob.publicKey);
    const b = await basketOf(w);
    assert.ok(b.control.smart);
    assert.equal(b.community, 1);
    const s: any = await w.ctx.vault.account.smart.fetch(pda.smart(w.basket));
    assert.deepEqual(Array.from(s.mandateHash), mandate);

    // The creator has no say.
    await expectError(rpc(w.ctx, w.ctx.vault.methods.proposeMandate(mandate, GUARD, true).accountsPartial({ creator: w.creator.publicKey, basket: w.basket, smart: pda.smart(w.basket) }), [w.creator]), "CommunityManaged");
    await expectError(rpc(w.ctx, w.ctx.vault.methods.setSmartPause(true).accountsPartial({ signer: w.creator.publicKey, protocol: pda.protocol(), basket: w.basket, smart: pda.smart(w.basket) }), [w.creator]), "NotPauser");
    await expectError(rpc(w.ctx, w.ctx.vault.methods.setTargets([5_000, 5_000]).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }), [w.creator]), "SmartManaged");

    // A vote can't reweight while the strategist runs it, but can change its mandate.
    await expectError(propose(w, w.bob, await reweight(w, [5_000, 5_000])), "BadProposal");
    const next = hash('{"goal":"value"}');
    const m = await passed(w, { mandate: { mandateHash: next, guard: { ...GUARD, maxDayBps: 1_000 } } });
    await executeSmart(w, m);
    await close(w, m, w.bob.publicKey);
    const s2: any = await w.ctx.vault.account.smart.fetch(pda.smart(w.basket));
    assert.deepEqual(Array.from(s2.mandateHash), next);
    assert.equal(s2.guard.maxDayBps, 1_000);

    // Leaving takes 66% and returns it to the creator, the rules account's rent too.
    const leave = await passed(w, { leave: {} });
    await expectError(execute(w, leave), "NotExecutable");
    const rent = w.ctx.svm.getBalance(w.creator.publicKey)!;
    await executeSmart(w, leave);
    assert.ok(w.ctx.svm.getBalance(w.creator.publicKey)! > rent);
    assert.ok((await basketOf(w)).control.creator);
  });

  it("guardrails that don't hold together can't be proposed (smart baskets have no protocol ceiling since 8 Nov 2026)", async () => {
    const w = await goCommunity();
    await bobReady(w);
    await expectError(propose(w, w.bob, { goSmart: { mandateHash: hash("x"), guard: { ...GUARD, maxDayBps: 6_000, maxWeekBps: 5_000 } } }), "BadGuardrails");
    await propose(w, w.bob, { goSmart: { mandateHash: hash("x"), guard: { ...GUARD, maxDayBps: 4_001, maxWeekBps: 12_000 } } });
  });
});

describe("community: at 14 assets", () => {
  it("proposes, votes and runs a reweight of 14 assets in one transaction each", async () => {
    const w = await goCommunity();
    await bobReady(w);
    for (let i = 0; i < 12; i++) {
      const { C, vault } = await assetC(w);
      const id = await propose(w, w.bob, { addAsset: { mint: C } });
      await vote(w, w.bob, id);
      warp(w.ctx, VOTE + WAIT);
      await executeAdd(w, id, C, vault);
      await close(w, id, w.bob.publicKey);
    }
    assert.equal((await basketOf(w)).constituentCount, 14);
    const next = [3_200, 2_800, 500, 500, 300, 300, 300, 300, 300, 300, 300, 300, 300, 300];
    assert.equal(next.reduce((a, b) => a + b), 10_000);
    const id = await passed(w, await reweight(w, next));
    await execute(w, id, await verifiedOf(w));
    assert.deepEqual(await targets(w), next);
  });
});

describe("community: handed over", () => {
  it("handing it to its holders is the manager's, and the fee keeps streaming to the manager", async () => {
    const w = await communityWorld();
    await openDoor(w);
    const carol = fund(w.ctx);
    await rpc(w.ctx, w.ctx.vault.methods.handOver(carol.publicKey).accountsPartial({ manager: w.creator.publicKey, basket: w.basket }), [w.creator]);
    await expectError(enter(w), "NotMandateAuthority");
    await enter(w, carol);
    assert.equal((await basketOf(w)).community, 1);
    // A community basket's manager still holds its fee, and can hand that on.
    const dave = fund(w.ctx);
    await rpc(w.ctx, w.ctx.vault.methods.handOver(dave.publicKey).accountsPartial({ manager: carol.publicKey, basket: w.basket }), [carol]);
    warp(w.ctx, 30 * DAY);
    const daveShares = await createAta(w.ctx, w.shareMint, dave.publicKey, w.shareProgram);
    await rpc(
      w.ctx,
      w.ctx.vault.methods
        .accrueFees()
        .accountsPartial({ basket: w.basket, shareMint: w.shareMint, creatorShares: daveShares, shareTokenProgram: w.shareProgram })
        .remainingAccounts([w.vaultA, w.vaultB].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false }))),
    );
    assert.ok(balance(w.ctx, daveShares) > 0n);
  });
});

describe("community: the creator's cut goes to share stakers", () => {
  const CUT = 60;
  /** A cut of 0.6%, raised before the basket is handed over, and Carol, who buys. */
  async function cutWorld() {
    const w = await communityWorld();
    await openDoor(w);
    await rpc(w.ctx, w.ctx.vault.methods.proposeCutRise(CUT).accountsPartial({ authority: w.creator.publicKey, basket: w.basket, notice: pda.cutNotice(w.basket) }), [w.creator]);
    warp(w.ctx, 2 * DAY);
    await rpc(w.ctx, w.ctx.vault.methods.applyCutRise().accountsPartial({ basket: w.basket, notice: pda.cutNotice(w.basket), creator: w.creator.publicKey }));
    const carol = fund(w.ctx);
    await fundHolder(w, carol);
    return { ...w, carol };
  }
  type CW = Awaited<ReturnType<typeof cutWorld>>;
  const buy = (w: CW, shares: bigint) => rpc(w.ctx, mintIx(w, w.carol, shares), [w.carol]);
  const claimStake = (w: CW, who: Keypair) =>
    rpc(
      w.ctx,
      w.ctx.vault.methods.claimStakeCut().accountsPartial({
        wallet: who.publicKey,
        basket: w.basket,
        shareMint: w.shareMint,
        ext: pda.ext(w.basket),
        escrow: ata(w.shareMint, pda.shareEscrow(), w.shareProgram),
        shareStake: pda.shareStake(w.basket, who.publicKey),
        protocol: pda.protocol(),
        treasuryShares: feeShares(w.shareMint, w.shareProgram).treasuryShares,
        walletShares: ata(w.shareMint, who.publicKey, w.shareProgram),
        shareTokenProgram: w.shareProgram,
      }),
      [who],
    );
  const claimCreator = (w: CW, ext: PublicKey | null = pda.ext(w.basket)) =>
    rpc(
      w.ctx,
      w.ctx.vault.methods.claimCreatorCut().accountsPartial({
        basket: w.basket,
        protocol: pda.protocol(),
        shareMint: w.shareMint,
        treasuryShares: feeShares(w.shareMint, w.shareProgram).treasuryShares,
        creatorShares: ata(w.shareMint, w.creator.publicKey, w.shareProgram),
        shareTokenProgram: w.shareProgram,
        ext,
      }),
    );
  const sharesOf = (w: CW, who: PublicKey) => balance(w.ctx, ata(w.shareMint, who, w.shareProgram));

  it("before the handover it's the creator's; after, the stakers', pro rata to what they stake", async () => {
    const w = await cutWorld();
    await buy(w, 100n * SHARES); // 0.6 shares to the creator
    await enter(w);
    await shareStake(w, w.bob, 500n * SHARES);
    await shareStake(w, w.alice, 50n * SHARES);
    await buy(w, 100n * SHARES); // 0.6 shares to the stakers: 500/550 Bob, 50/550 Alice
    // The creator takes only what was theirs before; leaving out the record is refused.
    await expectError(claimCreator(w, null), "BadExt");
    const c0 = sharesOf(w, w.creator.publicKey);
    await claimCreator(w);
    assert.equal(sharesOf(w, w.creator.publicKey) - c0, 600_000_000n);
    // What the app shows each staker (web/src/lib/stakeCut.ts) is what the program pays.
    const { cutOwed, readCutRec, readStakeTail } = await import("../web/src/lib/stakeCut.ts");
    const b = (await basketOf(w)) as any;
    const shown = (who: Keypair, amount: bigint) =>
      cutOwed({
        rec: readCutRec(w.ctx.svm.getAccount(pda.ext(w.basket))!.data)!,
        byVote: true,
        creatorUnclaimed: BigInt(b.creatorUnclaimed.toString()),
        staked: balance(w.ctx, ata(w.shareMint, pda.shareEscrow(), w.shareProgram)),
        amount,
        ...readStakeTail(w.ctx.svm.getAccount(pda.shareStake(w.basket, who.publicKey))!.data),
      });
    const [showBob, showAlice] = [shown(w.bob, 500n * SHARES), shown(w.alice, 50n * SHARES)];
    const [b0, a0] = [sharesOf(w, w.bob.publicKey), sharesOf(w, w.alice.publicKey)];
    await claimStake(w, w.bob);
    await claimStake(w, w.alice);
    const [bob, alice] = [sharesOf(w, w.bob.publicKey) - b0, sharesOf(w, w.alice.publicKey) - a0];
    assert.equal(showBob, bob, "the app's figure for Bob");
    assert.equal(showAlice, alice, "the app's figure for Alice");
    assert.ok(bob >= 545_454_544n && bob <= 545_454_546n, `bob ${bob}`);
    assert.ok(alice >= 54_545_453n && alice <= 54_545_455n, `alice ${alice}`);
    // Claimed in full: claiming again pays nothing.
    await claimStake(w, w.bob);
    assert.equal(sharesOf(w, w.bob.publicKey) - b0, bob);
  });

  it("the cut that accrued with nobody staked is the treasury's, not the first staker's (bug bounty, 10 Nov)", async () => {
    const w = await cutWorld();
    await enter(w);
    await buy(w, 100n * SHARES); // 0.6 shares accrue, nobody staked
    const owed = BigInt((await basketOf(w)).creatorUnclaimed.toString());
    await shareStake(w, w.bob, 90n * SHARES);
    // The stake's sync moved the backlog out of what's owed (to the treasury's shares).
    assert.equal(BigInt((await basketOf(w)).creatorUnclaimed.toString()), owed - 600_000_000n);
    const b0 = sharesOf(w, w.bob.publicKey);
    await claimStake(w, w.bob);
    assert.equal(sharesOf(w, w.bob.publicKey) - b0, 0n);
  });

  it("a stake joining later doesn't share what accrued before it; one leaving keeps what it earned", async () => {
    const w = await cutWorld();
    await enter(w);
    await shareStake(w, w.bob, 90n * SHARES);
    await buy(w, 100n * SHARES); // all 0.6 to Bob
    await shareStake(w, w.alice, 90n * SHARES);
    await buy(w, 100n * SHARES); // 0.3 each
    // Bob takes all his shares back: the account stays until he claims what he's owed.
    unlock(w, w.bob);
    await shareUnstake(w, w.bob, 90n * SHARES);
    assert.ok((w.ctx.svm.getAccount(pda.shareStake(w.basket, w.bob.publicKey))?.lamports ?? 0) > 0);
    const [b0, a0] = [sharesOf(w, w.bob.publicKey), sharesOf(w, w.alice.publicKey)];
    await claimStake(w, w.bob);
    await claimStake(w, w.alice);
    // Rounded down per staked share, a base unit at most.
    assert.ok(900_000_000n - (sharesOf(w, w.bob.publicKey) - b0) <= 1n);
    assert.ok(300_000_000n - (sharesOf(w, w.alice.publicKey) - a0) <= 1n);
    assert.equal(w.ctx.svm.getAccount(pda.shareStake(w.basket, w.bob.publicKey))?.lamports ?? 0, 0, "closed once paid");
  });

  it("leaving keeps what the stakers earned theirs; after it, the cut is the creator's again", async () => {
    const w = await cutWorld();
    await enter(w);
    await bobReady(w); // 900 staked
    await buy(w, 100n * SHARES); // 0.6 to the stakers, not shared yet
    await execute(w, await passed(w, { leave: {} }));
    assert.ok((await basketOf(w)).control.creator);
    await buy(w, 100n * SHARES); // 0.6 to the creator
    const c0 = sharesOf(w, w.creator.publicKey);
    await claimCreator(w);
    assert.equal(sharesOf(w, w.creator.publicKey) - c0, 600_000_000n, "only what accrued after leaving");
    const b0 = sharesOf(w, w.bob.publicKey);
    await claimStake(w, w.bob);
    // Rounded down per staked share: a base unit of dust stays with the stakers' record.
    assert.ok(600_000_000n - (sharesOf(w, w.bob.publicKey) - b0) <= 1n);
  });

  it("a basket vote-run before the upgrade starts its record with sync_stake_cut", async () => {
    const w = await cutWorld();
    await enter(w);
    // As if entered on the old program: no record yet.
    const acct = w.ctx.svm.getAccount(w.basket)!;
    const data = Buffer.from(acct.data);
    data[94] &= ~4;
    w.ctx.svm.setAccount(w.basket, { ...acct, data });
    await buy(w, 100n * SHARES); // before the record: still the creator's
    await rpc(
      w.ctx,
      w.ctx.vault.methods.syncStakeCut().accountsPartial({ payer: w.ctx.payer.publicKey, basket: w.basket, shareMint: w.shareMint, ext: pda.ext(w.basket), escrow: ata(w.shareMint, pda.shareEscrow(), w.shareProgram) }),
    );
    await shareStake(w, w.bob, 500n * SHARES);
    await buy(w, 100n * SHARES);
    const c0 = sharesOf(w, w.creator.publicKey);
    await claimCreator(w);
    assert.equal(sharesOf(w, w.creator.publicKey) - c0, 600_000_000n);
    const b0 = sharesOf(w, w.bob.publicKey);
    await claimStake(w, w.bob);
    assert.equal(sharesOf(w, w.bob.publicKey) - b0, 600_000_000n);
  });
});

describe("community: closing an emptied community basket", () => {
  it("waits for every proposal to be closed, then closes the community state and the extension account too", async () => {
    const w = await goCommunity({ creatorFeeBps: 0 });
    await bobReady(w);
    const id = await propose(w, w.bob, { split: { numerator: 2, denominator: 1 } });
    warp(w.ctx, VOTE + WAIT);
    await shareUnstake(w, w.bob, 900n * SHARES);
    for (const [who, n] of [[w.creator, 10n], [w.alice, 90n], [w.bob, 900n]] as const) await rpc(w.ctx, redeemIx(w, who, n * SHARES), [who]);
    assert.equal(supply(w.ctx, w.shareMint), 0n);
    await closeVaults(w, w.creator);
    // The USDC pot of a basket that's been vote-run is named too (never created here, so nothing to close).
    const side = [pda.ext(w.basket), pda.community(w.basket), potOf(w.basket)];
    // The lapsed proposal still counts as open until someone closes it (its rent is its proposer's).
    await expectError(closeBasket(w, w.creator, side), "BasketBusy");
    await close(w, id, w.bob.publicKey);
    await closeBasket(w, w.creator, side);
    for (const k of [w.basket, ...side.slice(0, 2)]) assert.ok(gone(w, k));
  });
});

describe("community: the cut in USDC goes to share stakers too", () => {
  // The team, 9 Nov 2026: "community basket fees go to holders staking the shares of the basket, those
  // have to be paid in native as well".
  const CUT = 60;
  const DOLLAR = 1_000_000n;
  async function usdcWorld(protocolFeeBps = 40) {
    const { usdcMint, usdcSides, feeBlock, quoteIx } = await import("./_usdc.ts");
    const w = await communityWorld({ protocolFeeBps });
    await openDoor(w);
    await rpc(w.ctx, w.ctx.vault.methods.proposeCutRise(CUT).accountsPartial({ authority: w.creator.publicKey, basket: w.basket, notice: pda.cutNotice(w.basket) }), [w.creator]);
    warp(w.ctx, 2 * DAY);
    await rpc(w.ctx, w.ctx.vault.methods.applyCutRise().accountsPartial({ basket: w.basket, notice: pda.cutNotice(w.basket), creator: w.creator.publicKey }));
    usdcMint(w.ctx);
    const sides = await usdcSides(w.ctx);
    const priceKey = fund(w.ctx);
    await initKeepers(w.ctx, priceKey.publicKey, priceKey.publicKey);
    const pot = await createAta(w.ctx, USDC, potOwner(w.basket));
    const carol = fund(w.ctx);
    await fundHolder(w, carol);
    const carolUsdc = await createAta(w.ctx, USDC, carol.publicKey);
    await mintTo(w.ctx, USDC, carolUsdc, 1_000n * DOLLAR);
    /** Carol buys `shares` worth $`dollars`, the cut to `cut` (the pot by default). */
    const buy = async (shares: bigint, dollars: bigint, cut = pot) => {
      const b = mintIx(w, carol, shares).remainingAccounts(feeBlock(sides, carol.publicKey, cut));
      await send(w.ctx, [await quoteIx(w.ctx, priceKey, dollars * DOLLAR), await b.instruction()], [carol, priceKey]);
    };
    return { ...w, sides, priceKey, pot, carol, carolUsdc, buy };
  }
  type UW = Awaited<ReturnType<typeof usdcWorld>>;
  const claimUsdc = async (w: UW, who: Keypair) => {
    const to = ata(USDC, who.publicKey);
    if (!w.ctx.svm.getAccount(to)) await createAta(w.ctx, USDC, who.publicKey);
    await rpc(
      w.ctx,
      w.ctx.vault.methods.claimStakeCutUsdc().accountsPartial({
        wallet: who.publicKey,
        basket: w.basket,
        shareMint: w.shareMint,
        ext: pda.ext(w.basket),
        escrow: ata(w.shareMint, pda.shareEscrow(), w.shareProgram),
        shareStake: pda.shareStake(w.basket, who.publicKey),
        pot: w.pot,
        walletUsdc: to,
      }),
      [who],
    );
    return balance(w.ctx, to);
  };

  it("what comes in with nobody staked is the treasury's: the first staker doesn't take it (bug bounty, 10 Nov)", async () => {
    const w = await usdcWorld();
    await enter(w);
    await w.buy(100n * SHARES, 100n); // $0.60 into the pot, nobody staked
    await shareStake(w, w.bob, 500n * SHARES);
    assert.equal(await claimUsdc(w, w.bob), 0n, "none of the backlog");
    const treasury = balance(w.ctx, w.sides.treasury);
    await rpc(
      w.ctx,
      w.ctx.vault.methods.potToTreasury().accountsPartial({
        basket: w.basket,
        shareMint: w.shareMint,
        ext: pda.ext(w.basket),
        escrow: ata(w.shareMint, pda.shareEscrow(), w.shareProgram),
        pot: w.pot,
        treasuryUsdc: w.sides.treasury,
      }),
    );
    assert.equal(balance(w.ctx, w.sides.treasury) - treasury, 600_000n);
    // From then on the stakers share it as before.
    await w.buy(100n * SHARES, 100n);
    assert.equal(await claimUsdc(w, w.bob), 600_000n);
  });

  it("before the handover the cut is the creator's USDC; after, the stakers', pro rata", async () => {
    const w = await usdcWorld();
    const creatorUsdc = await createAta(w.ctx, USDC, w.creator.publicKey);
    await w.buy(100n * SHARES, 100n, creatorUsdc);
    assert.equal(balance(w.ctx, creatorUsdc), 600_000n, "$0.60 straight to the creator");
    await enter(w);
    await shareStake(w, w.bob, 500n * SHARES);
    await shareStake(w, w.alice, 50n * SHARES);
    // Vote-run: the cut to the creator's account is refused as a USDC cut (kept in shares instead).
    await w.buy(100n * SHARES, 100n);
    assert.equal(balance(w.ctx, w.pot), 600_000n);
    assert.equal(balance(w.ctx, creatorUsdc), 600_000n, "nothing more to the creator");
    assert.equal(await claimUsdc(w, w.bob), (600_000n * 500n) / 550n);
    assert.equal(await claimUsdc(w, w.alice), (600_000n * 50n) / 550n);
    // Claiming again pays nothing more.
    assert.equal(await claimUsdc(w, w.bob), (600_000n * 500n) / 550n);
  });

  it("a stake joining later doesn't share what came before it; one leaving keeps what it earned", async () => {
    const w = await usdcWorld();
    await enter(w);
    await shareStake(w, w.bob, 80n * SHARES);
    await w.buy(100n * SHARES, 100n); // $0.60, all Bob's
    await shareStake(w, w.alice, 80n * SHARES);
    await w.buy(100n * SHARES, 100n); // $0.60, half each
    unlock(w, w.bob);
    await shareUnstake(w, w.bob, 80n * SHARES);
    // Bob unstaked everything but is still owed USDC: his stake account stays until he claims.
    assert.ok(w.ctx.svm.getAccount(pda.shareStake(w.basket, w.bob.publicKey)));
    await w.buy(100n * SHARES, 100n); // all Alice's
    assert.equal(await claimUsdc(w, w.bob), 600_000n + 300_000n);
    assert.equal(w.ctx.svm.getAccount(pda.shareStake(w.basket, w.bob.publicKey))?.lamports ?? 0, 0, "paid in full: closed");
    assert.equal(await claimUsdc(w, w.alice), 300_000n + 600_000n);
  });

  it("a cut aimed anywhere but the pot falls back to shares, counted for the stakers", async () => {
    const w = await usdcWorld();
    await enter(w);
    await shareStake(w, w.bob, 500n * SHARES);
    const before = (await basketOf(w)).creatorUnclaimed.toString();
    await w.buy(100n * SHARES, 100n, w.carolUsdc);
    assert.equal(balance(w.ctx, w.pot), 0n);
    assert.ok(BigInt((await basketOf(w)).creatorUnclaimed.toString()) > BigInt(before));
  });

  it("staking without the pot is refused once the basket has a cut record", async () => {
    const w = await usdcWorld();
    await enter(w);
    await expectError(
      rpc(
        w.ctx,
        w.ctx.vault.methods.shareStake(new BN(SHARES.toString())).accountsPartial({
          wallet: w.bob.publicKey,
          basket: w.basket,
          shareMint: w.shareMint,
          walletShares: ata(w.shareMint, w.bob.publicKey, w.shareProgram),
          ext: pda.ext(w.basket),
          escrow: ata(w.shareMint, pda.shareEscrow(), w.shareProgram),
          shareStake: pda.shareStake(w.basket, w.bob.publicKey),
          shareTokenProgram: w.shareProgram,
        }),
        [w.bob],
      ),
      "BadPot",
    );
  });

  it("closing the basket waits for the pot to be claimed, then closes it too", async () => {
    // No protocol fee here, so no fee shares are left to keep the supply above 0: the cut alone, in USDC.
    const w = await usdcWorld(0);
    await enter(w);
    await shareStake(w, w.bob, 500n * SHARES);
    await w.buy(100n * SHARES, 100n);
    unlock(w, w.bob);
    await shareUnstake(w, w.bob, 500n * SHARES);
    // Everyone out.
    for (const who of [w.creator, w.alice, w.bob, w.carol]) {
      const held = balance(w.ctx, ata(w.shareMint, who.publicKey, w.shareProgram));
      if (held > 0n) await rpc(w.ctx, redeemIx(w, who, held), [who]);
    }
    assert.equal(supply(w.ctx, w.shareMint), 0n);
    await closeVaults(w, w.creator);
    const side = [pda.ext(w.basket), pda.community(w.basket), w.pot, potOwner(w.basket), TOKEN_PROGRAM_ID];
    await expectError(closeBasket(w, w.creator, side), "BasketNotEmpty");
    await claimUsdc(w, w.bob);
    assert.equal(balance(w.ctx, w.pot), 0n);
    // Anyone sending dust to the pot can't block the close: with nothing owed past dust, what's left
    // goes to a USDC account of the manager's, named alongside.
    await mintTo(w.ctx, USDC, w.pot, 5n);
    await expectError(closeBasket(w, w.creator, side), "BadPot");
    const creatorUsdc = await createAta(w.ctx, USDC, w.creator.publicKey);
    await closeBasket(w, w.creator, [...side, creatorUsdc]);
    assert.equal(balance(w.ctx, creatorUsdc), 5n);
    for (const k of [w.basket, pda.ext(w.basket), w.pot]) assert.ok(gone(w, k));
  });

  it("on a vote-run basket the manager's own buy pays the cut too: it's the stakers'", async () => {
    const { feeBlock, quoteIx } = await import("./_usdc.ts");
    const w = await usdcWorld();
    await enter(w);
    await shareStake(w, w.bob, 500n * SHARES);
    const creatorUsdc = await createAta(w.ctx, USDC, w.creator.publicKey);
    await mintTo(w.ctx, USDC, creatorUsdc, 10n * DOLLAR);
    await fundHolder(w, w.creator);
    const b = mintIx(w, w.creator, 10n * SHARES).remainingAccounts(feeBlock(w.sides, w.creator.publicKey, w.pot));
    await send(w.ctx, [await quoteIx(w.ctx, w.priceKey, 10n * DOLLAR), await b.instruction()], [w.creator, w.priceKey]);
    assert.equal(balance(w.ctx, w.pot), 60_000n, "0.60% of $10 to the stakers' pot");
  });
});
