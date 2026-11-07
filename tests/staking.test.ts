// GRAVI staking: a stake of the threshold or more shares stakers' 10% of protocol fees, in SOL paid
// out per second, and burns no GRAVI creating baskets. Perks start two days after the stake reaches
// the threshold, and unstaking takes seven days, earning nothing.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { Keypair, PublicKey, SYSVAR_INSTRUCTIONS_PUBKEY } from "@solana/web3.js";
import { MINT_SIZE, MintLayout, NATIVE_MINT, createTransferCheckedInstruction } from "@solana/spl-token";
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
  now,
  pda,
  rpc,
  send,
  supply,
  warp,
  type Ctx,
} from "./helpers.ts";
import { NO_GRAVI, feeShares, fundHolder, listAsset, mintIx, setCreationBurn, world } from "./fixtures.ts";

const GRAVI_PROGRAM = TOKEN_2022_PROGRAM_ID;
const ONE_GRAVI = 1_000_000n; // mainnet's: Token-2022, 6 decimals
const MILLION = 1_000_000n * ONE_GRAVI;
const DAY = 86_400;
const MONTH = 30 * DAY;
const SHARES = 1_000_000_000n;
const n = (b: { toString(): string }) => BigInt(b.toString());

function nativeMint(ctx: Ctx) {
  const data = Buffer.alloc(MINT_SIZE);
  MintLayout.encode({ mintAuthorityOption: 0, mintAuthority: PublicKey.default, supply: 0n, decimals: 9, isInitialized: true, freezeAuthorityOption: 0, freezeAuthority: PublicKey.default }, data);
  ctx.svm.setAccount(NATIVE_MINT, { lamports: Number(ctx.svm.minimumBalanceForRentExemption(BigInt(MINT_SIZE))), data, owner: TOKEN_PROGRAM_ID, executable: false });
}

async function stakeWorld(opts: { on?: boolean } = {}) {
  const w = await world({ gravi: { program: GRAVI_PROGRAM, decimals: 6 } });
  const { ctx } = w;
  warp(ctx, 1_000_000); // LiteSVM's clock starts at 0, which a real one never reads
  await rpc(ctx, ctx.vault.methods.setProtocolFees(40, 5_000).accountsPartial({ authority: ctx.payer.publicKey, protocol: pda.protocol() }));
  nativeMint(ctx);
  const keeper = fund(ctx);
  await rpc(ctx, ctx.vault.methods.initKeepers(keeper.publicKey, keeper.publicKey).accountsPartial({ authority: ctx.payer.publicKey, keepers: pda.keepers() }));
  const poolGravi = await createAta(ctx, w.graviMint, pda.stakePool(), GRAVI_PROGRAM);
  const pot = await createAta(ctx, NATIVE_MINT, pda.stakeRewards(), TOKEN_PROGRAM_ID);
  const keeperSol = await createAta(ctx, NATIVE_MINT, keeper.publicKey, TOKEN_PROGRAM_ID);
  const s = { ...w, keeper, poolGravi, pot, keeperSol };
  if (opts.on ?? true) await initStaking(s);
  return s;
}
type SW = Awaited<ReturnType<typeof stakeWorld>>;

const initStaking = (w: Pick<SW, "ctx" | "graviMint">, authority = w.ctx.payer) =>
  rpc(w.ctx, w.ctx.vault.methods.initStaking().accountsPartial({ authority: authority.publicKey, protocol: pda.protocol(), graviMint: w.graviMint, pool: pda.stakePool() }), [authority]);

/** A wallet holding `gravi` GRAVI. */
async function staker(w: SW, gravi = 5n * MILLION) {
  const who = fund(w.ctx);
  const acct = await createAta(w.ctx, w.graviMint, who.publicKey, GRAVI_PROGRAM);
  await mintTo(w.ctx, w.graviMint, acct, gravi, GRAVI_PROGRAM);
  await createAta(w.ctx, NATIVE_MINT, who.publicKey, TOKEN_PROGRAM_ID);
  return who;
}

const stake = (w: SW, who: Keypair, amount: bigint) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.stake(new BN(amount.toString())).accountsPartial({
      wallet: who.publicKey,
      pool: pda.stakePool(),
      stake: pda.stake(who.publicKey),
      graviMint: w.graviMint,
      walletGravi: ata(w.graviMint, who.publicKey, GRAVI_PROGRAM),
      poolGravi: w.poolGravi,
      graviTokenProgram: GRAVI_PROGRAM,
    }),
    [who],
  );

const unstake = (w: SW, who: Keypair, amount: bigint) =>
  rpc(w.ctx, w.ctx.vault.methods.unstake(new BN(amount.toString())).accountsPartial({ wallet: who.publicKey, pool: pda.stakePool(), stake: pda.stake(who.publicKey) }), [who]);

const withdraw = (w: SW, who: Keypair) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.stakeWithdraw().accountsPartial({
      wallet: who.publicKey,
      pool: pda.stakePool(),
      stake: pda.stake(who.publicKey),
      graviMint: w.graviMint,
      walletGravi: ata(w.graviMint, who.publicKey, GRAVI_PROGRAM),
      poolGravi: w.poolGravi,
      graviTokenProgram: GRAVI_PROGRAM,
    }),
    [who],
  );

const refresh = (w: SW, who: Keypair) => rpc(w.ctx, w.ctx.vault.methods.stakeRefresh().accountsPartial({ pool: pda.stakePool(), stake: pda.stake(who.publicKey) }));

const claim = (w: SW, who: Keypair) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.stakeClaim().accountsPartial({
      wallet: who.publicKey,
      pool: pda.stakePool(),
      stake: pda.stake(who.publicKey),
      nativeMint: NATIVE_MINT,
      pot: w.pot,
      walletSol: ata(NATIVE_MINT, who.publicKey, TOKEN_PROGRAM_ID),
      tokenProgram: TOKEN_PROGRAM_ID,
    }),
    [who],
  );

const fundPot = (w: SW) => rpc(w.ctx, w.ctx.vault.methods.stakeFund().accountsPartial({ pool: pda.stakePool(), pot: w.pot }));

/** Anyone adds SOL to the pot by sending it wrapped SOL. */
async function deposit(w: SW, lamports: bigint) {
  w.ctx.svm.airdrop(w.pot, lamports);
  const data = Buffer.from(w.ctx.svm.getAccount(w.pot)!.data);
  data.writeBigUInt64LE(data.readBigUInt64LE(64) + lamports, 64);
  const acc = w.ctx.svm.getAccount(w.pot)!;
  w.ctx.svm.setAccount(w.pot, { ...acc, data });
}

const pool = (w: SW) => w.ctx.vault.account.stakePool.fetch(pda.stakePool()) as Promise<any>;
const stakeOf = (w: SW, who: Keypair) => w.ctx.vault.account.stake.fetch(pda.stake(who.publicKey)) as Promise<any>;
const owed = async (w: SW) => n((await w.ctx.vault.account.basket.fetch(w.basket)).stakeOwed as BN);

/** A stake past its warm-up, switched on. */
async function warmed(w: SW, gravi = MILLION) {
  const who = await staker(w);
  await stake(w, who, gravi);
  warp(w.ctx, 2 * DAY);
  await refresh(w, who);
  return who;
}

describe("staking: the fee split", () => {
  it("every fee owes stakers 10% once staking is on, out of the treasury's side", async () => {
    const w = await stakeWorld({ on: false });
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await rpc(w.ctx, mintIx(w, alice, 100n * SHARES), [alice]);
    assert.equal(await owed(w), 0n, "nothing before it's on");
    const { burnShares, treasuryShares } = feeShares(w.shareMint);
    const burn0 = balance(w.ctx, burnShares);
    const tre0 = balance(w.ctx, treasuryShares);

    await initStaking(w);
    await rpc(w.ctx, mintIx(w, alice, 100n * SHARES), [alice]);
    const fee = (100n * SHARES * 40n) / 10_000n; // 0.4 share
    assert.equal(balance(w.ctx, burnShares) - burn0, fee / 2n, "the burn half doesn't change");
    assert.equal(balance(w.ctx, treasuryShares) - tre0, fee / 2n, "the shares sit in the treasury");
    assert.equal(await owed(w), fee / 10n, "10% of the fee is owed: burn 50, stakers 10, treasury 40");
  });

  it("only the protocol authority turns it on, once, and only then can anyone stake", async () => {
    const w = await stakeWorld({ on: false });
    const who = await staker(w);
    await expectError(stake(w, who, MILLION), "AccountNotInitialized");
    await expectError(initStaking(w, fund(w.ctx)), "NotProtocolAuthority");
    await initStaking(w);
    await expectError(initStaking(w), "already in use");
    assert.equal(n((await pool(w)).threshold), MILLION);
  });

  it("the treasury can't withdraw what stakers are owed; the sweep moves it, for anyone", async () => {
    const w = await stakeWorld();
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await rpc(w.ctx, mintIx(w, alice, 100n * SHARES), [alice]);
    const { treasuryShares } = feeShares(w.shareMint);
    const dest = await createAta(w.ctx, w.shareMint, w.ctx.payer.publicKey);
    const take = (amount: bigint) =>
      rpc(w.ctx, w.ctx.vault.methods.treasuryWithdraw(new BN(amount.toString())).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol(), basket: w.basket, shareMint: w.shareMint, treasuryShares, destination: dest, shareTokenProgram: TOKEN_PROGRAM_ID }));
    const held = balance(w.ctx, treasuryShares);
    const due = await owed(w);
    await expectError(take(held - due + 1n), "TreasuryOwedToStakers");
    await take(held - due);
    assert.equal(balance(w.ctx, treasuryShares), due);

    const rewardShares = await createAta(w.ctx, w.shareMint, pda.stakeRewards());
    const sweep = () => rpc(w.ctx, w.ctx.vault.methods.stakeSweep().accountsPartial({ basket: w.basket, shareMint: w.shareMint, protocol: pda.protocol(), treasuryShares, reward_shares: rewardShares, rewardShares, shareTokenProgram: TOKEN_PROGRAM_ID }));
    await sweep();
    assert.equal(balance(w.ctx, rewardShares), due);
    assert.equal(await owed(w), 0n);
    await expectError(sweep(), "NothingToTake");
  });
});

describe("staking: next to the creator's cut", () => {
  it("the sweep leaves what the creator is owed, and the treasury keeps back both", async () => {
    const w = await stakeWorld();
    await rpc(w.ctx, w.ctx.vault.methods.proposeCutRise(60).accountsPartial({ authority: w.creator.publicKey, basket: w.basket, notice: pda.cutNotice(w.basket) }), [w.creator]);
    warp(w.ctx, 2 * DAY);
    await rpc(w.ctx, w.ctx.vault.methods.applyCutRise().accountsPartial({ basket: w.basket, notice: pda.cutNotice(w.basket), creator: w.creator.publicKey }));
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await rpc(w.ctx, mintIx(w, alice, 100n * SHARES), [alice]);
    const b = await w.ctx.vault.account.basket.fetch(w.basket);
    const cut = n(b.creatorUnclaimed as BN);
    const due = n(b.stakeOwed as BN);
    assert.equal(cut, (100n * SHARES * 60n) / 10_000n, "the cut is counted");
    assert.equal(due, ((100n * SHARES * 40n) / 10_000n) / 10n, "stakers' 10% is of the protocol fee, not the cut");
    const { treasuryShares } = feeShares(w.shareMint);
    const held = balance(w.ctx, treasuryShares);
    assert.equal(held, cut + (100n * SHARES * 40n) / 10_000n / 2n);
    const dest = await createAta(w.ctx, w.shareMint, w.ctx.payer.publicKey);
    const take = (amount: bigint) =>
      rpc(w.ctx, w.ctx.vault.methods.treasuryWithdraw(new BN(amount.toString())).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol(), basket: w.basket, shareMint: w.shareMint, treasuryShares, destination: dest, shareTokenProgram: TOKEN_PROGRAM_ID }));
    await expectError(take(held - cut + 1n), "TreasuryOwedToCreator");
    await expectError(take(held - cut - due + 1n), "TreasuryOwedToStakers");
    await take(held - cut - due);
    assert.equal(balance(w.ctx, treasuryShares), cut + due);

    const rewardShares = await createAta(w.ctx, w.shareMint, pda.stakeRewards());
    await rpc(w.ctx, w.ctx.vault.methods.stakeSweep().accountsPartial({ basket: w.basket, shareMint: w.shareMint, protocol: pda.protocol(), treasuryShares, rewardShares, shareTokenProgram: TOKEN_PROGRAM_ID }));
    assert.equal(balance(w.ctx, rewardShares), due);
    assert.equal(balance(w.ctx, treasuryShares), cut, "the creator's stays for the creator");
  });
});

describe("staking: locks", () => {
  it("any amount counts toward the pot 2 days after it's staked", async () => {
    const w = await stakeWorld();
    const who = await staker(w);
    await stake(w, who, 1n);
    warp(w.ctx, 2 * DAY - 1);
    await refresh(w, who);
    assert.equal(n((await pool(w)).totalCounted), 0n, "not a second before 2 days");
    warp(w.ctx, 1);
    await refresh(w, who);
    assert.equal(n((await pool(w)).totalCounted), 1n);
  });

  it("a top-up waits its own 2 days; what already counts keeps counting", async () => {
    const w = await stakeWorld();
    const who = await warmed(w);
    await stake(w, who, MILLION);
    assert.equal(n((await pool(w)).totalCounted), MILLION);
    warp(w.ctx, 2 * DAY);
    await refresh(w, who);
    assert.equal(n((await pool(w)).totalCounted), 2n * MILLION);
  });

  it("unstaking takes what's still warming first, stops it earning at once, and pays out after 7 days", async () => {
    const w = await stakeWorld();
    const who = await warmed(w);
    await stake(w, who, MILLION);
    await unstake(w, who, MILLION);
    let s = await stakeOf(w, who);
    assert.equal(n(s.pending), 0n, "the top-up went first");
    assert.equal(n(s.counted), MILLION, "what counted still counts");
    const before = balance(w.ctx, ata(w.graviMint, who.publicKey, GRAVI_PROGRAM));
    await unstake(w, who, MILLION);
    assert.equal(n((await pool(w)).totalCounted), 0n);
    await expectError(withdraw(w, who), "UnstakeWaiting");
    warp(w.ctx, 7 * DAY - 1);
    await expectError(withdraw(w, who), "UnstakeWaiting");
    warp(w.ctx, 1);
    await withdraw(w, who);
    assert.equal(balance(w.ctx, ata(w.graviMint, who.publicKey, GRAVI_PROGRAM)) - before, 2n * MILLION);
    await expectError(withdraw(w, who), "NothingToTake");
    s = await stakeOf(w, who);
    assert.equal(n(s.amount), 0n);
    await rpc(w.ctx, w.ctx.vault.methods.stakeClose().accountsPartial({ wallet: who.publicKey, stake: pda.stake(who.publicKey) }), [who]);
    assert.equal(w.ctx.svm.getAccount(pda.stake(who.publicKey))?.lamports ?? 0, 0);
  });

  it("can't unstake more than is staked, or someone else's", async () => {
    const w = await stakeWorld();
    const who = await warmed(w);
    await expectError(unstake(w, who, MILLION + 1n), "NotEnoughStaked");
    const other = fund(w.ctx);
    await expectError(
      rpc(w.ctx, w.ctx.vault.methods.unstake(new BN(1)).accountsPartial({ wallet: other.publicKey, pool: pda.stakePool(), stake: pda.stake(who.publicKey) }), [other]),
      "ConstraintSeeds",
    );
  });

  it("the threshold: lower applies now, higher after a week, under a 10M ceiling, authority only", async () => {
    const w = await stakeWorld();
    const set = (amount: bigint, authority = w.ctx.payer) =>
      rpc(w.ctx, w.ctx.vault.methods.setStakeThreshold(new BN(amount.toString())).accountsPartial({ authority: authority.publicKey, protocol: pda.protocol(), pool: pda.stakePool() }), [authority]);
    await expectError(set(5n * MILLION, fund(w.ctx)), "NotProtocolAuthority");
    await expectError(set(10n * MILLION + 1n), "BadStakeThreshold");
    await expectError(set(0n), "BadStakeThreshold");
    await set(2n * MILLION);
    assert.equal(n((await pool(w)).threshold), MILLION, "a raise waits out its notice");
    assert.equal(n((await pool(w)).nextThreshold), 2n * MILLION);
    await set(MILLION / 2n);
    const p = await pool(w);
    assert.equal(n(p.threshold), MILLION / 2n, "a lower one applies now");
    assert.equal(n(p.nextThresholdAt), 0n, "and replaces the raise");
  });
});

describe("staking: the pot", () => {
  const SOL = 1_000_000_000n;

  it("pays per second, in proportion, and a stake claims any time", async () => {
    const w = await stakeWorld();
    const a = await warmed(w);
    const b = await warmed(w, 3n * MILLION);
    await refresh(w, a);
    await deposit(w, 30n * SOL);
    await fundPot(w);

    warp(w.ctx, MONTH / 2);
    await claim(w, a);
    await claim(w, b);
    const solA = balance(w.ctx, ata(NATIVE_MINT, a.publicKey, TOKEN_PROGRAM_ID));
    const solB = balance(w.ctx, ata(NATIVE_MINT, b.publicKey, TOKEN_PROGRAM_ID));
    const near = (got: bigint, want: bigint) => assert.ok(got <= want && want - got < 1_000n, `${got} vs ${want}`);
    near(solA, (15n * SOL) / 4n);
    near(solB, (15n * SOL * 3n) / 4n);

    await expectError(claim(w, a), "NothingToTake");
    warp(w.ctx, MONTH); // past the end: no more than the pot
    await claim(w, a);
    await claim(w, b);
    near(balance(w.ctx, ata(NATIVE_MINT, a.publicKey, TOKEN_PROGRAM_ID)), (30n * SOL) / 4n);
    near(balance(w.ctx, ata(NATIVE_MINT, b.publicKey, TOKEN_PROGRAM_ID)), (30n * SOL * 3n) / 4n);
    assert.ok(balance(w.ctx, w.pot) < 2_000n, "the pot pays out, dust aside");
  });

  it("a minute staked earns a minute: hopping in just before gains nothing", async () => {
    const w = await stakeWorld();
    const long = await warmed(w);
    await deposit(w, 30n * SOL);
    await fundPot(w);
    warp(w.ctx, MONTH - 3 * DAY);
    const hop = await staker(w);
    await stake(w, hop, 4n * MILLION);
    warp(w.ctx, 2 * DAY);
    await refresh(w, hop);
    // It counts for the last day only, at 4/5 of what that day pays.
    warp(w.ctx, DAY);
    await claim(w, hop);
    const got = balance(w.ctx, ata(NATIVE_MINT, hop.publicKey, TOKEN_PROGRAM_ID));
    const day = SOL; // 30 SOL over 30 days
    assert.ok(got <= (day * 4n) / 5n && (day * 4n) / 5n - got < 1_000n, `${got}`);
    await claim(w, long);
    assert.ok(balance(w.ctx, ata(NATIVE_MINT, long.publicKey, TOKEN_PROGRAM_ID)) > 29n * SOL);
  });

  it("what nothing counted to earn goes back to the next pot", async () => {
    const w = await stakeWorld();
    await deposit(w, 30n * SOL);
    await fundPot(w);
    await expectError(fundPot(w), "NothingToTake");
    warp(w.ctx, MONTH);
    assert.equal(n((await pool(w)).periods), 1n);
    await fundPot(w); // nobody counted: all of it again
    const p = await pool(w);
    assert.equal(n(p.periods), 2n);
    assert.equal(n(p.funded), 30n * SOL);

    const empty = await stakeWorld();
    await expectError(fundPot(empty), "NothingToTake");
  });

  it("a dust donation can't hold the month's SOL back: a top-up re-spreads over a fresh month (L3)", async () => {
    const w = await stakeWorld();
    const a = await warmed(w);
    // A griefer starts a month with 1 lamport before the keeper's sales land.
    await deposit(w, 1n);
    await fundPot(w);
    warp(w.ctx, DAY);
    // The real SOL arrives and tops the month up: it all streams over the next 30 days.
    await deposit(w, 30n * SOL);
    await fundPot(w);
    const p = await pool(w);
    assert.equal(n(p.funded), 30n * SOL + 1n);
    assert.equal(n(p.periods), 1n, "a top-up isn't a new month");
    // Another dust top-up would stretch the payout out: it must bring at least what has streamed.
    warp(w.ctx, DAY);
    await deposit(w, 1n);
    await expectError(fundPot(w), "StreamRunning");
    warp(w.ctx, MONTH);
    await claim(w, a);
    const got = balance(w.ctx, ata(NATIVE_MINT, a.publicKey, TOKEN_PROGRAM_ID));
    assert.ok(got <= 30n * SOL + 1n && 30n * SOL - got < 1_000n, `${got}`);
  });

  it("a new pot never promises what stakers are still owed", async () => {
    const w = await stakeWorld();
    const a = await warmed(w);
    await deposit(w, 30n * SOL);
    await fundPot(w);
    warp(w.ctx, MONTH);
    await deposit(w, 3n * SOL);
    await fundPot(w);
    assert.ok(n((await pool(w)).funded) === 33n * SOL);
    warp(w.ctx, MONTH);
    await claim(w, a);
    const got = balance(w.ctx, ata(NATIVE_MINT, a.publicKey, TOKEN_PROGRAM_ID));
    assert.ok(got <= 33n * SOL && 33n * SOL - got < 1_000n, `${got}`);
  });
});

describe("staking: selling for the pot", () => {
  const PRICE_A = 1_000_000_000_000n; // $1
  const PRICE_SOL = 100_000_000_000_000n; // $100

  async function salesWorld() {
    const w = await stakeWorld();
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await rpc(w.ctx, mintIx(w, alice, 10_000n * SHARES), [alice]);
    const { treasuryShares } = feeShares(w.shareMint);
    const rewardShares = await createAta(w.ctx, w.shareMint, pda.stakeRewards());
    await rpc(w.ctx, w.ctx.vault.methods.stakeSweep().accountsPartial({ basket: w.basket, shareMint: w.shareMint, protocol: pda.protocol(), treasuryShares, rewardShares, shareTokenProgram: TOKEN_PROGRAM_ID }));
    const rewardA = await createAta(w.ctx, w.mintA, pda.stakeRewards(), TOKEN_PROGRAM_ID);
    const rewardB = await createAta(w.ctx, w.mintB, pda.stakeRewards(), TOKEN_2022_PROGRAM_ID);
    const keeperA = await createAta(w.ctx, w.mintA, w.keeper.publicKey, TOKEN_PROGRAM_ID);
    const post = (mint: PublicKey, price: bigint) => rpc(w.ctx, w.ctx.vault.methods.setPrice(new BN(price.toString())).accountsPartial({ priceAuthority: w.keeper.publicKey, keepers: pda.keepers(), mint, assetPrice: pda.price(mint), pythPrice: null }), [w.keeper]);
    await post(w.mintA, PRICE_A);
    await post(NATIVE_MINT, PRICE_SOL);
    return { ...w, rewardShares, rewardA, rewardB, keeperA, post };
  }
  type W = Awaited<ReturnType<typeof salesWorld>>;

  const redeem = (w: W, shares: bigint) =>
    rpc(
      w.ctx,
      w.ctx.vault.methods
        .stakeRedeem(new BN(shares.toString()))
        .accountsPartial({ basket: w.basket, shareMint: w.shareMint, rewardShares: w.rewardShares, shareTokenProgram: TOKEN_PROGRAM_ID })
        .remainingAccounts(
          [
            [w.vaultA, w.rewardA, w.mintA, TOKEN_PROGRAM_ID],
            [w.vaultB, w.rewardB, w.mintB, TOKEN_2022_PROGRAM_ID],
          ].flatMap((row) => row.map((pubkey, i) => ({ pubkey, isSigner: false, isWritable: i < 2 }))),
        ),
    );

  const startIx = (w: W, amount: bigint, slippage = 100, refund = 0n, signer = w.keeper, source = w.rewardA) =>
    w.ctx.vault.methods
      .stakeSellStart(new BN(amount.toString()), slippage, new BN(refund.toString()))
      .accountsPartial({
        rebalancer: signer.publicKey,
        keepers: pda.keepers(),
        pool: pda.stakePool(),
        sellMint: w.mintA,
        sellSource: source,
        rebalancerSell: w.keeperA,
        sellPrice: pda.price(w.mintA),
        solPrice: pda.price(NATIVE_MINT),
        pot: w.pot,
        sellTokenProgram: TOKEN_PROGRAM_ID,
        instructions: SYSVAR_INSTRUCTIONS_PUBKEY,
      })
      .instruction();
  const finishIx = (w: W, signer = w.keeper) =>
    w.ctx.vault.methods.stakeSellFinish().accountsPartial({ rebalancer: signer.publicKey, keepers: pda.keepers(), pool: pda.stakePool(), pot: w.pot }).instruction();
  const deliver = (w: W, lamports: bigint) => createTransferCheckedInstruction(w.keeperSol, NATIVE_MINT, w.pot, w.keeper.publicKey, lamports, 9, [], TOKEN_PROGRAM_ID);

  async function wrapped(w: W, lamports: bigint) {
    w.ctx.svm.airdrop(w.keeperSol, lamports);
    const acc = w.ctx.svm.getAccount(w.keeperSol)!;
    const data = Buffer.from(acc.data);
    data.writeBigUInt64LE(data.readBigUInt64LE(64) + lamports, 64);
    w.ctx.svm.setAccount(w.keeperSol, { ...acc, data });
  }

  it("redeems swept shares and sells the tokens for SOL into the pot, against the oracle", async () => {
    const w = await salesWorld();
    const shares = balance(w.ctx, w.rewardShares);
    const sup = supply(w.ctx, w.shareMint);
    await redeem(w, shares);
    assert.equal(balance(w.ctx, w.rewardShares), 0n);
    assert.equal(supply(w.ctx, w.shareMint), sup - shares);
    const held = balance(w.ctx, w.rewardA);
    assert.ok(held > 0n);

    await wrapped(w, 1_000_000_000n);
    const worth = (held * 1_000_000_000n) / 1_000_000n / 100n; // token A at $1, SOL at $100
    await expectError(send(w.ctx, [await startIx(w, held), deliver(w, (worth * 98n) / 100n), await finishIx(w)], [w.keeper]), "BelowOracleFloor");
    await send(w.ctx, [await startIx(w, held), deliver(w, (worth * 99n) / 100n), await finishIx(w)], [w.keeper]);
    assert.equal(balance(w.ctx, w.pot), (worth * 99n) / 100n);
    assert.equal(n((await pool(w)).totalSol), (worth * 99n) / 100n);

    // Now empty, its account closes and the rent goes back.
    const before = w.ctx.svm.getBalance(w.keeper.publicKey)!;
    await rpc(w.ctx, w.ctx.vault.methods.stakeCloseAccount().accountsPartial({ rebalancer: w.keeper.publicKey, keepers: pda.keepers(), mint: w.mintA, account: w.rewardA, tokenProgram: TOKEN_PROGRAM_ID }), [w.keeper]);
    assert.ok(w.ctx.svm.getBalance(w.keeper.publicKey)! > before);
  });

  it("only the rebalancer sells, a start needs its finish, and it never reaches staked GRAVI", async () => {
    const w = await salesWorld();
    await redeem(w, balance(w.ctx, w.rewardShares));
    const held = balance(w.ctx, w.rewardA);
    const thief = fund(w.ctx);
    await expectError(send(w.ctx, [await startIx(w, held, 100, 0n, thief), await finishIx(w, thief)], [thief]), "NotRebalancer");
    await expectError(send(w.ctx, [await startIx(w, held)], [w.keeper]), "BuybackFinishMissing");

    // Staked GRAVI sits with the pool, not the rewards PDA: nothing that sells can name it.
    const who = await staker(w);
    await stake(w, who, MILLION);
    await w.post(w.graviMint, 1_000_000_000n);
    const steal = await w.ctx.vault.methods
      .stakeSellStart(new BN(MILLION.toString()), 100, new BN(0))
      .accountsPartial({
        rebalancer: w.keeper.publicKey,
        keepers: pda.keepers(),
        pool: pda.stakePool(),
        sellMint: w.graviMint,
        sellSource: w.poolGravi,
        rebalancerSell: w.keeperA,
        sellPrice: pda.price(w.graviMint),
        solPrice: pda.price(NATIVE_MINT),
        pot: w.pot,
        sellTokenProgram: GRAVI_PROGRAM,
        instructions: SYSVAR_INSTRUCTIONS_PUBKEY,
      })
      .instruction();
    await expectError(send(w.ctx, [steal, await finishIx(w)], [w.keeper]), "ConstraintTokenOwner");
    await expectError(
      rpc(w.ctx, w.ctx.vault.methods.stakeCloseAccount().accountsPartial({ rebalancer: w.keeper.publicKey, keepers: pda.keepers(), mint: NATIVE_MINT, account: w.pot, tokenProgram: TOKEN_PROGRAM_ID }), [w.keeper]),
      "BurnAccountKept",
    );
  });
});

describe("staking: free baskets", () => {
  const BURN = 10_000n * ONE_GRAVI;

  async function createBasket(w: SW, who: Keypair, gravi: boolean, stakeAccounts?: Keypair, writable = true, mints = [w.mintA, w.mintB]) {
    const basketId = Keypair.generate().publicKey.toBytes().slice(0, 16);
    const basket = pda.basket(who.publicKey, basketId);
    const extra = stakeAccounts
      ? [
          { pubkey: pda.stake(stakeAccounts.publicKey), isSigner: false, isWritable: writable },
          { pubkey: pda.stakePool(), isSigner: false, isWritable: false },
        ]
      : [];
    await rpc(
      w.ctx,
      w.ctx.vault.methods
        .initializeBasket({ basketId: Array.from(basketId), access: { public: {} }, control: { creator: {} }, rebalanceMode: { hybrid: {} }, intervalSecs: new BN(86_400), driftBps: 500, mgmtFeeBps: 20, mints, targetBps: mints.map(() => 10_000 / mints.length) })
        .accountsPartial({
          creator: who.publicKey,
          basket,
          shareMint: pda.share(basket),
          shareTokenProgram: TOKEN_PROGRAM_ID,
          ...(gravi ? { graviMint: w.graviMint, creatorGravi: ata(w.graviMint, who.publicKey, GRAVI_PROGRAM), graviTokenProgram: GRAVI_PROGRAM } : NO_GRAVI),
        })
        .remainingAccounts([
          ...mints.flatMap((m) => [m, pda.asset(m)]).map((pubkey) => ({ pubkey, isSigner: false, isWritable: false })),
          ...extra,
        ]),
      [who],
    );
    return basket;
  }

  const usedToday = async (w: SW, who: Keypair) => (await stakeOf(w, who)).freeUsed as number;

  it("1M warmed up makes one basket a day with no burn; GRAVI still warming doesn't count", async () => {
    const w = await stakeWorld();
    await setCreationBurn(w.ctx, w.graviMint, BURN);
    const who = await staker(w);
    await stake(w, who, MILLION);
    const wallet = ata(w.graviMint, who.publicKey, GRAVI_PROGRAM);
    const left = balance(w.ctx, wallet);

    await expectError(createBasket(w, who, false, who), "CreationBurnAccountsMissing");
    await createBasket(w, who, true, who);
    assert.equal(balance(w.ctx, wallet), left - 2n * BURN, "warming up: burned");

    warp(w.ctx, 2 * DAY);
    await createBasket(w, who, false, who); // no refresh needed: the clock says so
    assert.equal(await usedToday(w, who), 1);
    await expectError(createBasket(w, who, false, who), "CreationBurnAccountsMissing");
    await createBasket(w, who, true, who);
    assert.equal(balance(w.ctx, wallet), left - 4n * BURN, "the second of the day burns");

    warp(w.ctx, DAY);
    await createBasket(w, who, false, who);
    assert.equal(await usedToday(w, who), 1, "a new day, a new one");
  });

  it("2M makes two a day, 2.9M still two, and adding an asset uses one", async () => {
    const w = await stakeWorld();
    await setCreationBurn(w.ctx, w.graviMint, BURN);
    const who = await staker(w);
    await stake(w, who, (29n * MILLION) / 10n);
    warp(w.ctx, 2 * DAY);
    const basket = await createBasket(w, who, false, who);
    // A listed third asset, its vault ready, added with the stake: the day's second.
    const C = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
    await listAsset(w.ctx, C);
    await createAta(w.ctx, C, basket, TOKEN_PROGRAM_ID);
    const add = (gravi: boolean) =>
      rpc(
        w.ctx,
        w.ctx.vault.methods
          .addConstituent()
          .accountsPartial({
            authority: who.publicKey,
            basket,
            mint: C,
            assetEntry: pda.asset(C),
            vault: ata(C, basket, TOKEN_PROGRAM_ID),
            ...(gravi ? { graviMint: w.graviMint, creatorGravi: ata(w.graviMint, who.publicKey, GRAVI_PROGRAM), graviTokenProgram: GRAVI_PROGRAM } : NO_GRAVI),
          })
          .remainingAccounts([
            { pubkey: pda.stake(who.publicKey), isSigner: false, isWritable: true },
            { pubkey: pda.stakePool(), isSigner: false, isWritable: false },
          ]),
        [who],
      );
    await add(false);
    assert.equal(await usedToday(w, who), 2);
    await expectError(createBasket(w, who, false, who), "CreationBurnAccountsMissing");
  });

  it("the stake must be passed writable, and nothing due or the exempt wallet uses no free basket", async () => {
    const w = await stakeWorld();
    const who = await warmed(w);
    // Creation burn off: nothing is due, so nothing is used.
    await createBasket(w, who, false, who);
    assert.equal(await usedToday(w, who), 0);
    await setCreationBurn(w.ctx, w.graviMint, BURN);
    await expectError(createBasket(w, who, false, who, false), "BadStake");
  });

  it("making a basket smart while creating it is free with a free basket a day, without using one", async () => {
    const w = await stakeWorld();
    await setCreationBurn(w.ctx, w.graviMint, BURN);
    await rpc(w.ctx, w.ctx.vault.methods.setSmartBurn(new BN((50_000n * ONE_GRAVI).toString())).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol(), graviMint: w.graviMint }));
    const who = await warmed(w);
    // Four assets at 25%, within the guardrails (two can't be: 35% each is 70%).
    const more: PublicKey[] = [];
    for (let i = 0; i < 2; i++) {
      const m = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
      await listAsset(w.ctx, m);
      more.push(m);
    }
    const basket = await createBasket(w, who, false, who, true, [w.mintA, w.mintB, ...more]);
    const guard = { maxDayBps: 2_000, maxWeekBps: 5_000, maxWeightBps: 3_500, maxUnverifiedBps: 3_000, minAssets: 2, maxAssets: 6, stopBps: 0, stopTrailing: false };
    const before = balance(w.ctx, ata(w.graviMint, who.publicKey, GRAVI_PROGRAM));
    await rpc(
      w.ctx,
      w.ctx.vault.methods
        .enableSmart(Array.from(new Uint8Array(32).fill(7)), guard)
        .accountsPartial({ creator: who.publicKey, basket, smart: pda.smart(basket), protocol: pda.protocol(), creationBurn: pda.creationBurn(), ...NO_GRAVI })
        .remainingAccounts([
          { pubkey: pda.stake(who.publicKey), isSigner: false, isWritable: false },
          { pubkey: pda.stakePool(), isSigner: false, isWritable: false },
        ]),
      [who],
    );
    assert.equal(balance(w.ctx, ata(w.graviMint, who.publicKey, GRAVI_PROGRAM)), before);
    assert.equal(await usedToday(w, who), 1, "still only the creation's");
  });

  it("unstaking ends it at once, and someone else's stake doesn't help", async () => {
    const w = await stakeWorld();
    await setCreationBurn(w.ctx, w.graviMint, BURN);
    const who = await warmed(w);
    const other = await staker(w);
    await expectError(createBasket(w, other, true, who), "BadStake");
    await unstake(w, who, MILLION);
    await expectError(createBasket(w, who, false, who), "CreationBurnAccountsMissing");
  });

  it("a fake stake account isn't believed", async () => {
    const w = await stakeWorld();
    await setCreationBurn(w.ctx, w.graviMint, BURN);
    const who = await warmed(w);
    const fake = Keypair.generate();
    const acc = w.ctx.svm.getAccount(pda.stake(who.publicKey))!;
    w.ctx.svm.setAccount(fake.publicKey, { ...acc, owner: fund(w.ctx).publicKey });
    const basketId = new Uint8Array(16).fill(9);
    const basket = pda.basket(who.publicKey, basketId);
    await expectError(
      rpc(
        w.ctx,
        w.ctx.vault.methods
          .initializeBasket({ basketId: Array.from(basketId), access: { public: {} }, control: { creator: {} }, rebalanceMode: { hybrid: {} }, intervalSecs: new BN(86_400), driftBps: 500, mgmtFeeBps: 20, mints: [w.mintA, w.mintB], targetBps: [5_000, 5_000] })
          .accountsPartial({ creator: who.publicKey, basket, shareMint: pda.share(basket), shareTokenProgram: TOKEN_PROGRAM_ID, ...NO_GRAVI })
          .remainingAccounts([w.mintA, pda.asset(w.mintA), w.mintB, pda.asset(w.mintB), fake.publicKey, pda.stakePool()].map((pubkey) => ({ pubkey, isSigner: false, isWritable: true }))),
        [who],
      ),
      "BadStake",
    );
  });
});
