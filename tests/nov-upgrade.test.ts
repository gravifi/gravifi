// The upgrade that drops the protocol's Smart ceilings and lets a part prove itself (`verify_part`),
// rehearsed: a smart basket made and run by the program on mainnet (target/deploy/onchain.so:
// `solana program dump -u m HHmD4ZP5he9GrcmcNrpjahyK1dWh2MhMwmVpiPRJsw3G target/deploy/onchain.so`),
// then this build loaded over it. Skipped without the dump.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { PublicKey, VAULT_ID, expectError, pda, rpc, warp } from "./helpers.ts";
import { ROLE, setRoleIx, setParam, PARAM, PARAM_NOTICE, potOf } from "./fixtures.ts";
import { DAY, GUARD, MANDATE, OFT, enable, named, setTargets, targets, verifyPart, wirePart, world, type Guard, type W } from "./_parts.ts";

const ONCHAIN = new URL("../target/deploy/onchain.so", import.meta.url).pathname;
const BUILD_SO = new URL("../target/deploy/gravifi_vault.so", import.meta.url).pathname;
/** The dump is this build already (the release re-run after its upgrade): the dump's first bytes
 *  are the build's. */
function alreadyUpgraded(): boolean {
  if (!existsSync(ONCHAIN) || !existsSync(BUILD_SO)) return false;
  const build = readFileSync(BUILD_SO);
  const onchain = readFileSync(ONCHAIN);
  return onchain.length >= build.length && onchain.subarray(0, build.length).equals(build);
}
const BUILD = new URL("../target/deploy/gravifi_vault.so", import.meta.url).pathname;

const propose = (w: W, guard: Guard) =>
  rpc(w.ctx, w.ctx.vault.methods.proposeMandate(MANDATE.map((x) => x ^ 1), guard, false).accountsPartial({ creator: w.creator.publicKey, basket: w.basket, smart: pda.smart(w.basket) }), [w.creator]);
const apply = (w: W) => rpc(w.ctx, w.ctx.vault.methods.applyMandate().accountsPartial({ basket: w.basket, smart: pda.smart(w.basket), creator: w.creator.publicKey }));
const smartOf = (w: W) => w.ctx.vault.account.smart.fetch(pda.smart(w.basket)) as Promise<{ guard: Guard; changes: number; mandateHash: number[] }>;

describe("November upgrade: from the program on mainnet", () => {
  it("a smart basket made before keeps its guardrails and turnover, and may then go past the old ceilings", async (t) => {
    if (!existsSync(ONCHAIN)) return t.skip("no dump of the mainnet program at target/deploy/onchain.so");
    if (alreadyUpgraded()) return t.skip("the program on mainnet is already this build: nothing to rehearse");
    const w = await world([2_500, 2_500, 2_000, 3_000], ONCHAIN);
    const wide: Guard = { maxDayBps: 6_000, maxWeekBps: 20_000, maxWeightBps: 5_000, maxUnverifiedBps: 4_500, minAssets: 2, maxAssets: 6, stopBps: 0, stopTrailing: false };

    // On today's program: smart within the 40% / 100% / 35% / 30% ceilings, a review applied, a
    // wider mandate refused, and no parts role.
    await enable(w, GUARD);
    await setTargets(w, [3_000, 2_000, 2_000, 3_000]);
    await expectError(propose(w, wide), "BadGuardrails");
    await expectError(rpc(w.ctx, setRoleIx(w.ctx, ROLE.partAdmin, OFT)), "BadRole");
    // A ceiling param lowered on today's program (it binds vote-run baskets after the upgrade).
    await setParam(w.ctx, PARAM.VOTE_MAX_DAY_BPS, 1_000n);
    const before = await smartOf(w);
    const raw = Buffer.from(w.ctx.svm.getAccount(pda.smart(w.basket))!.data);

    // The upgrade.
    w.ctx.svm.addProgramFromFile(VAULT_ID, BUILD);
    warp(w.ctx, PARAM_NOTICE);

    // The rules account reads as before, byte for byte.
    assert.deepEqual(Buffer.from(w.ctx.svm.getAccount(pda.smart(w.basket))!.data), raw, "untouched by the upgrade");
    const after = await smartOf(w);
    assert.deepEqual(after.guard, before.guard);
    assert.equal(after.changes, 1);
    // Still held to its own guardrails: the part at its creator's 30% Unverified cap.
    await expectError(setTargets(w, [2_500, 2_000, 2_000, 3_500]), "WeightAboveCap");
    await setTargets(w, [2_000, 2_500, 2_500, 3_000]);

    // The wider mandate now passes (the lowered param is the vote ceiling, not a smart one).
    await propose(w, wide);
    warp(w.ctx, DAY);
    await apply(w);
    assert.deepEqual((await smartOf(w)).guard, wide);
    await setTargets(w, [500, 500, 4_500, 4_500]); // 45 points in one review, the part at 45%
    assert.deepEqual(await targets(w), [500, 500, 4_500, 4_500]);

    // The part proves itself, and may go to the weight cap.
    await expectError(setTargets(w, [500, 500, 4_000, 5_000]), "WeightAboveCap");
    await named(w);
    const { store, peer } = await wirePart(w.ctx, w.P, w.admin.publicKey);
    await verifyPart(w.ctx, w.P, store, peer, w.creator);
    await setTargets(w, [500, 500, 4_000, 5_000]);
    assert.deepEqual(await targets(w), [500, 500, 4_000, 5_000]);
  });
});

describe("November upgrade: a vote-run basket and its stakes from before", () => {
  it("old stakes read and top up, the record starts at the creator's due, and the cut then goes to stakers", async (t) => {
    if (!existsSync(ONCHAIN)) return t.skip("no dump of the mainnet program at target/deploy/onchain.so");
    const { world: baseWorld, fundHolder, mintIx, feeShares, setParamIx, CONFIG } = await import("./fixtures.ts");
    const { ata, balance, createAta, fund, send } = await import("./helpers.ts");
    const { BN } = await import("@coral-xyz/anchor");
    const { SystemProgram, TransactionInstruction } = await import("@solana/web3.js");
    const w = await baseWorld({ program: ONCHAIN });
    const { ctx } = w;
    warp(ctx, 1_000_000);
    const SH = 1_000_000_000n;
    const [alice, carol] = [fund(ctx), fund(ctx)];
    await fundHolder(w, alice);
    await fundHolder(w, carol);
    await rpc(ctx, mintIx(w, alice, 50n * SH), [alice]);
    await createAta(ctx, w.shareMint, pda.shareEscrow(), w.shareProgram);
    await rpc(ctx, setParamIx(ctx, CONFIG.communityOpen, 1));
    await rpc(ctx, ctx.vault.methods.proposeCutRise(60).accountsPartial({ authority: w.creator.publicKey, basket: w.basket, notice: pda.cutNotice(w.basket) }), [w.creator]);
    warp(ctx, 2 * 86_400);
    await rpc(ctx, ctx.vault.methods.applyCutRise().accountsPartial({ basket: w.basket, notice: pda.cutNotice(w.basket), creator: w.creator.publicKey }));

    // On today's program, with its own accounts (no extension account then).
    const ix = (name: string, args: object, keys: [PublicKey, boolean, boolean][]) =>
      new TransactionInstruction({ programId: VAULT_ID, keys: keys.map(([pubkey, isSigner, isWritable]) => ({ pubkey, isSigner, isWritable })), data: ctx.vault.coder.instruction.encode(name, args) });
    const PARAMS = PublicKey.findProgramAddressSync([Buffer.from("params")], VAULT_ID)[0];
    await send(ctx, [ix("enterCommunity", {}, [[w.creator.publicKey, true, true], [w.basket, false, true], [pda.community(w.basket), false, true], [pda.protocol(), false, false], [SystemProgram.programId, false, false], [PARAMS, false, false]])], [w.creator]);
    const escrow = ata(w.shareMint, pda.shareEscrow(), w.shareProgram);
    const oldStake = (amount: bigint) =>
      ix("shareStake", { amount: new BN(amount.toString()) }, [
        [alice.publicKey, true, true], [w.basket, false, false], [w.shareMint, false, false], [ata(w.shareMint, alice.publicKey, w.shareProgram), false, true],
        [pda.shareEscrow(), false, false], [escrow, false, true], [pda.shareStake(w.basket, alice.publicKey), false, true], [w.shareProgram, false, false], [SystemProgram.programId, false, false],
      ]);
    await send(ctx, [oldStake(20n * SH)], [alice]);
    assert.equal(ctx.svm.getAccount(pda.shareStake(w.basket, alice.publicKey))!.data.length, 97);
    await rpc(ctx, mintIx(w, carol, 100n * SH), [carol]); // 0.6 to the creator, as today
    const due = BigInt(((await ctx.vault.account.basket.fetch(w.basket)) as any).creatorUnclaimed.toString());
    assert.equal(due, 600_000_000n);

    // The upgrade.
    ctx.svm.addProgramFromFile(VAULT_ID, BUILD);
    warp(ctx, 60);

    // The release's sync starts the record: what's owed now stays the creator's.
    await rpc(ctx, ctx.vault.methods.syncStakeCut().accountsPartial({ payer: ctx.payer.publicKey, basket: w.basket, shareMint: w.shareMint, ext: pda.ext(w.basket), escrow }));
    // The old 97-byte stake tops up on the new program, and grows to hold its position.
    const stake = (amount: bigint) =>
      rpc(ctx, ctx.vault.methods.shareStake(new BN(amount.toString())).accountsPartial({ wallet: alice.publicKey, basket: w.basket, shareMint: w.shareMint, walletShares: ata(w.shareMint, alice.publicKey, w.shareProgram), ext: pda.ext(w.basket), escrow, shareStake: pda.shareStake(w.basket, alice.publicKey), shareTokenProgram: w.shareProgram }).remainingAccounts([{ pubkey: potOf(w.basket), isSigner: false, isWritable: false }]), [alice]);
    await stake(10n * SH);
    // Both positions: the cut in shares and the cut in USDC.
    assert.equal(ctx.svm.getAccount(pda.shareStake(w.basket, alice.publicKey))!.data.length, 145);
    assert.equal(((await ctx.vault.account.shareStake.fetch(pda.shareStake(w.basket, alice.publicKey))) as any).amount.toString(), (30n * SH).toString());
    await rpc(ctx, mintIx(w, carol, 100n * SH), [carol]); // 0.6 to the stakers: all Alice's
    const treasury = feeShares(w.shareMint, w.shareProgram).treasuryShares;
    const creatorShares = ata(w.shareMint, w.creator.publicKey, w.shareProgram);
    const c0 = balance(ctx, creatorShares);
    await rpc(ctx, ctx.vault.methods.claimCreatorCut().accountsPartial({ basket: w.basket, protocol: pda.protocol(), shareMint: w.shareMint, treasuryShares: treasury, creatorShares, shareTokenProgram: w.shareProgram, ext: pda.ext(w.basket) }));
    assert.equal(balance(ctx, creatorShares) - c0, 600_000_000n);
    const aliceShares = ata(w.shareMint, alice.publicKey, w.shareProgram);
    const a0 = balance(ctx, aliceShares);
    await rpc(ctx, ctx.vault.methods.claimStakeCut().accountsPartial({ wallet: alice.publicKey, basket: w.basket, shareMint: w.shareMint, ext: pda.ext(w.basket), escrow, shareStake: pda.shareStake(w.basket, alice.publicKey), protocol: pda.protocol(), treasuryShares: treasury, walletShares: aliceShares, shareTokenProgram: w.shareProgram }), [alice]);
    assert.ok(600_000_000n - (balance(ctx, aliceShares) - a0) <= 1n);
  });
});

describe("November upgrade: closing a basket made before", () => {
  it("a basket emptied on today's program closes after the upgrade, its side accounts with it", async (t) => {
    if (!existsSync(ONCHAIN)) return t.skip("no dump of the mainnet program at target/deploy/onchain.so");
    const { world: baseWorld, redeemIx, SEED_SHARES, feeShares } = await import("./fixtures.ts");
    const { fund, supply } = await import("./helpers.ts");
    const { closeBasket, closeVaults, gone, lamports } = await import("./_close.ts");
    const w = await baseWorld({ program: ONCHAIN, access: "allowlist", creatorFeeBps: 0 });
    const c = w.creator;
    const friend = fund(w.ctx).publicKey;
    await rpc(w.ctx, w.ctx.vault.methods.allowWallet().accountsPartial({ creator: c.publicKey, basket: w.basket, wallet: friend, allowEntry: pda.allow(w.basket, friend) }), [c]);
    await rpc(w.ctx, w.ctx.vault.methods.setStops([]).accountsPartial({ creator: c.publicKey, basket: w.basket, stopLoss: pda.stop(w.basket) }), [c]);
    await rpc(w.ctx, redeemIx(w, c, SEED_SHARES), [c]);
    assert.equal(supply(w.ctx, w.shareMint), 0n);
    // Today's program has no close.
    await expectError(closeVaults(w, c), "InstructionFallbackNotFound");

    w.ctx.svm.addProgramFromFile(VAULT_ID, BUILD);
    warp(w.ctx, 60);

    const side = [pda.allow(w.basket, friend), pda.stop(w.basket)];
    const { treasuryShares, burnShares } = feeShares(w.shareMint, w.shareProgram);
    const all = [w.vaultA, w.vaultB, w.basket, ...side, treasuryShares, burnShares];
    const rent = all.reduce((s, k) => s + lamports(w, k), 0n);
    const before = lamports(w, c.publicKey);
    await closeVaults(w, c);
    await closeBasket(w, c, side);
    for (const k of all) assert.ok(gone(w, k), `${k.toBase58()} closed`);
    assert.equal(lamports(w, c.publicKey) - before, rent, "every lamport of rent to the creator");
  });
});
