// Notices on the protocol authority's own settings: a key named for a role (`set_role`) acts only
// after a day, while the key in force keeps acting, and removing a role is at once; a fee or burn
// raise (`set_param` CONFIG_*) waits a week and then applies by itself, while lowering is at once.
// Each waiting change sits next to what it replaces, so every instruction that reads it names no new
// account, and accounts from before (shorter, nothing waiting) read exactly as they did.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import { BN, expectError, fund, now, pda, rpc, warp, type Ctx } from "./helpers.ts";
import {
  CONFIG,
  FEE_NOTICE,
  FEE_SLOT,
  PARAM,
  PENDING_ROLES_OFFSET,
  ROLE,
  ROLE_NOTICE,
  feeShares,
  fundHolder,
  initKeepers,
  mintIx,
  pendingFee,
  setParam,
  setRole,
  world,
  type World,
} from "./fixtures.ts";

const ONE_SHARE = 1_000_000_000n;
const E12 = 1_000_000_000_000n;

/** A world whose clock isn't at zero, with `keeper` as both keepers. */
async function noticeWorld(protocolFeeBps = 0) {
  const w = await world({ protocolFeeBps });
  warp(w.ctx, 1_000_000);
  const keeper = fund(w.ctx);
  await initKeepers(w.ctx, keeper.publicKey, keeper.publicKey);
  return { ...w, keeper };
}

const name = (ctx: Ctx, role: number, key: PublicKey, authority = ctx.payer) => setRole(ctx, role, key, authority, true);

const post = (w: World, signer: Keypair, mint = w.mintA) =>
  rpc(w.ctx, w.ctx.vault.methods.setPrice(new BN(E12.toString())).accountsPartial({ priceAuthority: signer.publicKey, keepers: pda.keepers(), mint, assetPrice: pda.price(mint), pythPrice: null }), [signer]);

const invite = (w: World, rebalancer: Keypair) => {
  const wallet = fund(w.ctx);
  return rpc(
    w.ctx,
    w.ctx.vault.methods.setInvite(new BN(now(w.ctx) + 86_400)).accountsPartial({ wallet: wallet.publicKey, rebalancer: rebalancer.publicKey, keepers: pda.keepers(), invite: pda.invite(wallet.publicKey) }),
    [wallet, rebalancer],
  );
};

const delist = (w: World, signer: Keypair) =>
  rpc(w.ctx, w.ctx.vault.methods.setAssetStatus({ delisted: {} }).accountsPartial({ signer: signer.publicKey, protocol: pda.protocol(), mint: w.mintB, assetEntry: pda.asset(w.mintB) }), [signer]);

/** A pending key slot: `[key 32][applies_at i64]`. */
function pendingKey(ctx: Ctx, account: PublicKey, at: number) {
  const d = Buffer.from(ctx.svm.getAccount(account)!.data);
  return { key: new PublicKey(d.subarray(at, at + 32)), at: Number(d.readBigInt64LE(at + 32)) };
}

describe("roles: a key named acts after a day", () => {
  it("a new price authority waits a day while the one in force keeps posting; then only the new one posts", async () => {
    const w = await noticeWorld();
    const next = fund(w.ctx);
    await name(w.ctx, ROLE.priceAuthority, next.publicKey);
    assert.deepEqual(pendingKey(w.ctx, pda.keepers(), 73), { key: next.publicKey, at: now(w.ctx) + ROLE_NOTICE });
    await expectError(post(w, next), "NotPriceAuthority");
    await post(w, w.keeper);
    warp(w.ctx, ROLE_NOTICE - 1);
    await expectError(post(w, next), "NotPriceAuthority");
    await post(w, w.keeper);
    warp(w.ctx, 1);
    await expectError(post(w, w.keeper), "NotPriceAuthority");
    await post(w, next);
    // The stored key catches up at the next set_role: the rebalancer's slot is untouched.
    await name(w.ctx, ROLE.rebalancer, w.keeper.publicKey);
    const k = await w.ctx.vault.account.keepers.fetch(pda.keepers());
    assert.ok(k.priceAuthority.equals(w.keeper.publicKey), "only the role named is folded");
    await setRole(w.ctx, ROLE.priceAuthority, next.publicKey);
    assert.ok((await w.ctx.vault.account.keepers.fetch(pda.keepers())).priceAuthority.equals(next.publicKey));
  });

  it("the rebalancer too: the old one signs for the day, the new one after", async () => {
    const w = await noticeWorld();
    const next = fund(w.ctx);
    await name(w.ctx, ROLE.rebalancer, next.publicKey);
    await expectError(invite(w, next), "NotRebalancer");
    await invite(w, w.keeper);
    warp(w.ctx, ROLE_NOTICE);
    await expectError(invite(w, w.keeper), "NotRebalancer");
    await invite(w, next);
  });

  it("removing a role is at once, and calls off a key waiting for it", async () => {
    const w = await noticeWorld();
    const next = fund(w.ctx);
    await name(w.ctx, ROLE.priceAuthority, next.publicKey);
    await name(w.ctx, ROLE.priceAuthority, PublicKey.default);
    await expectError(post(w, w.keeper), "NotPriceAuthority");
    warp(w.ctx, ROLE_NOTICE);
    await expectError(post(w, next), "NotPriceAuthority");
    assert.deepEqual(pendingKey(w.ctx, pda.keepers(), 73), { key: PublicKey.default, at: 0 });
  });

  it("naming the key in force withdraws a key waiting", async () => {
    const w = await noticeWorld();
    const next = fund(w.ctx);
    await name(w.ctx, ROLE.priceAuthority, next.publicKey);
    await name(w.ctx, ROLE.priceAuthority, w.keeper.publicKey);
    warp(w.ctx, ROLE_NOTICE);
    await expectError(post(w, next), "NotPriceAuthority");
    await post(w, w.keeper);
  });

  it("a new protocol authority waits a day; the old one keeps the protocol until then, and not after", async () => {
    const w = await noticeWorld();
    const next = fund(w.ctx);
    await name(w.ctx, ROLE.authority, next.publicKey);
    assert.deepEqual(pendingKey(w.ctx, pda.protocol(), PENDING_ROLES_OFFSET), { key: next.publicKey, at: now(w.ctx) + ROLE_NOTICE });
    await expectError(setParam(w.ctx, PARAM.PRICE_AGE_SECS, 60n, [], next), "NotProtocolAuthority");
    await setParam(w.ctx, PARAM.PRICE_AGE_SECS, 60n);
    warp(w.ctx, ROLE_NOTICE);
    await expectError(setParam(w.ctx, PARAM.PRICE_AGE_SECS, 90n), "NotProtocolAuthority");
    await expectError(delist(w, w.ctx.payer), "NotProtocolAuthority");
    await setParam(w.ctx, PARAM.PRICE_AGE_SECS, 90n, [], next);
    // Naming itself, the key in force, folds it into the protocol's field.
    await name(w.ctx, ROLE.authority, next.publicKey, next);
    assert.ok((await w.ctx.vault.account.protocol.fetch(pda.protocol())).authority.equals(next.publicKey));
    assert.deepEqual(pendingKey(w.ctx, pda.protocol(), PENDING_ROLES_OFFSET), { key: PublicKey.default, at: 0 });
  });

  it("a new guardian can't delist for a day; removing it is at once", async () => {
    const w = await noticeWorld();
    const guardian = fund(w.ctx);
    await name(w.ctx, ROLE.guardian, guardian.publicKey);
    await expectError(delist(w, guardian), "NotProtocolAuthority");
    warp(w.ctx, ROLE_NOTICE);
    await delist(w, guardian);
    await name(w.ctx, ROLE.guardian, PublicKey.default);
    await expectError(delist(w, guardian), "NotProtocolAuthority");
  });

  it("the lister, the strategist and the burn-exempt wallet wait their day the same way", async () => {
    const w = await noticeWorld();
    const lister = Keypair.generate().publicKey, strategist = Keypair.generate().publicKey, exempt = Keypair.generate().publicKey;
    await name(w.ctx, ROLE.lister, lister);
    await name(w.ctx, ROLE.strategist, strategist);
    await name(w.ctx, ROLE.burnExempt, exempt);
    const due = now(w.ctx) + ROLE_NOTICE;
    assert.deepEqual(pendingKey(w.ctx, pda.lister(), 41), { key: lister, at: due });
    assert.deepEqual(pendingKey(w.ctx, pda.strategist(), 41), { key: strategist, at: due });
    assert.deepEqual(pendingKey(w.ctx, pda.protocol(), PENDING_ROLES_OFFSET + 80), { key: exempt, at: due });
    // Nothing is in force yet: the accounts' own fields are still empty.
    assert.ok((await w.ctx.vault.account.strategist.fetch(pda.strategist())).strategist.equals(PublicKey.default));
    const raw = Buffer.from(w.ctx.svm.getAccount(pda.protocol())!.data);
    assert.ok(new PublicKey(raw.subarray(113, 145)).equals(PublicKey.default));
  });

  it("keys named before this release keep acting: older, shorter accounts read as they did, and grow when next set", async () => {
    const w = await noticeWorld();
    // As on mainnet today: the keepers' account is [discriminator][bump][price][rebalancer], 73 bytes.
    const acct = w.ctx.svm.getAccount(pda.keepers())!;
    w.ctx.svm.setAccount(pda.keepers(), { ...acct, data: Buffer.from(acct.data).subarray(0, 73) });
    // ...and the protocol account ends at its flags.
    const p = w.ctx.svm.getAccount(pda.protocol())!;
    w.ctx.svm.setAccount(pda.protocol(), { ...p, data: Buffer.from(p.data).subarray(0, 157) });
    await post(w, w.keeper);
    await invite(w, w.keeper);
    await setParam(w.ctx, PARAM.PRICE_AGE_SECS, 60n);
    // Naming a new rebalancer grows the account; the price authority stays, the rebalancer waits.
    const next = fund(w.ctx);
    await name(w.ctx, ROLE.rebalancer, next.publicKey);
    assert.equal(w.ctx.svm.getAccount(pda.keepers())!.data.length, 153);
    await post(w, w.keeper);
    await invite(w, w.keeper);
    await expectError(invite(w, next), "NotRebalancer");
  });
});

describe("fees and burns: a raise waits a week, lowering is at once", () => {
  /** The fee shares a mint of 10 shares by a fresh holder pays (burn + treasury). */
  async function mintFee(w: World) {
    const who = fund(w.ctx);
    await fundHolder(w, who);
    const { burnShares, treasuryShares } = feeShares(w.shareMint, w.shareProgram);
    const held = () => {
      const b = (k: PublicKey) => BigInt(Buffer.from(w.ctx.svm.getAccount(k)!.data).readBigUInt64LE(64));
      return b(burnShares) + b(treasuryShares);
    };
    const before = held();
    await rpc(w.ctx, mintIx(w, who, 10n * ONE_SHARE), [who]);
    return held() - before;
  }
  const fees = (fee: number, share = 5_000) => BigInt(fee) | (BigInt(share) << 16n);

  it("a protocol fee raise waits its week, mints paying the fee in force until then; then it applies by itself", async () => {
    const w = await noticeWorld(10);
    await setParam(w.ctx, CONFIG.protocolFees, fees(40), [], w.ctx.payer, true);
    let p = await w.ctx.vault.account.protocol.fetch(pda.protocol());
    assert.equal(p.protocolFeeBps, 10);
    assert.deepEqual(pendingFee(w.ctx, FEE_SLOT.protocolFees), { value: fees(40), at: BigInt(now(w.ctx) + FEE_NOTICE) });
    assert.equal(await mintFee(w), 10_000_000n); // 0.10% of 10 shares
    warp(w.ctx, FEE_NOTICE - 1);
    assert.equal(await mintFee(w), 10_000_000n);
    warp(w.ctx, 1);
    assert.equal(await mintFee(w), 40_000_000n); // 0.40%, with no transaction from the authority
    // The next change starts from the fee in force.
    await setParam(w.ctx, CONFIG.protocolFees, fees(30), [], w.ctx.payer, true);
    p = await w.ctx.vault.account.protocol.fetch(pda.protocol());
    assert.equal(p.protocolFeeBps, 30);
    assert.equal(pendingFee(w.ctx, FEE_SLOT.protocolFees).at, 0n);
    assert.equal(await mintFee(w), 30_000_000n);
  });

  it("lowering is at once and withdraws a raise waiting; so does setting the fee in force", async () => {
    const w = await noticeWorld(20);
    await setParam(w.ctx, CONFIG.protocolFees, fees(50), [], w.ctx.payer, true);
    await setParam(w.ctx, CONFIG.protocolFees, fees(5), [], w.ctx.payer, true);
    assert.equal((await w.ctx.vault.account.protocol.fetch(pda.protocol())).protocolFeeBps, 5);
    assert.equal(pendingFee(w.ctx, FEE_SLOT.protocolFees).at, 0n);
    await setParam(w.ctx, CONFIG.protocolFees, fees(50), [], w.ctx.payer, true);
    await setParam(w.ctx, CONFIG.protocolFees, fees(5), [], w.ctx.payer, true);
    warp(w.ctx, FEE_NOTICE);
    assert.equal(await mintFee(w), 5_000_000n);
  });

  it("burning more of the fee is a raise too: it waits; burning less is at once", async () => {
    const w = await noticeWorld(40);
    await setParam(w.ctx, CONFIG.protocolFees, fees(40, 7_000), [], w.ctx.payer, true);
    assert.equal((await w.ctx.vault.account.protocol.fetch(pda.protocol())).burnShareBps, 5_000);
    warp(w.ctx, FEE_NOTICE);
    await setParam(w.ctx, CONFIG.protocolFees, fees(40, 6_000), [], w.ctx.payer, true);
    assert.equal((await w.ctx.vault.account.protocol.fetch(pda.protocol())).burnShareBps, 6_000);
  });

  it("the Smart fee and the Smart burn: raises wait, lowering is at once, ceilings stay code", async () => {
    const w = await noticeWorld();
    const raw = () => Buffer.from(w.ctx.svm.getAccount(pda.protocol())!.data);
    await setParam(w.ctx, CONFIG.smartFee, 90n, [], w.ctx.payer, true);
    assert.equal(raw()[147], 1);
    assert.equal(raw().readUInt16LE(145), 80, "the default stays in force, written as the value");
    assert.equal(pendingFee(w.ctx, FEE_SLOT.smartFee).value, 90n);
    await expectError(setParam(w.ctx, CONFIG.smartFee, 101n, [], w.ctx.payer, true), "FeeTooHigh");
    await setParam(w.ctx, CONFIG.smartFee, 60n, [], w.ctx.payer, true);
    assert.equal(raw().readUInt16LE(145), 60);
    assert.equal(pendingFee(w.ctx, FEE_SLOT.smartFee).at, 0n);
    await setParam(w.ctx, CONFIG.smartBurn, 5n, [w.graviMint], w.ctx.payer, true);
    assert.equal(raw().readBigUInt64LE(148), 0n);
    assert.equal(pendingFee(w.ctx, FEE_SLOT.smartBurn).value, 5n);
    warp(w.ctx, FEE_NOTICE);
    // Lowering from the burn in force (5, its week passed) is at once.
    await setParam(w.ctx, CONFIG.smartBurn, 3n, [w.graviMint], w.ctx.payer, true);
    assert.equal(raw().readBigUInt64LE(148), 3n);
  });

  it("the creation burn: a raise waits, lowering is at once; its very first value, before any basket can exist, is at once", async () => {
    const w = await noticeWorld();
    const per = async () => BigInt((await w.ctx.vault.account.creationBurn.fetch(pda.creationBurn())).perAsset.toString());
    await setParam(w.ctx, CONFIG.creationBurn, 7n, [pda.creationBurn(), w.graviMint], w.ctx.payer, true);
    assert.equal(await per(), 0n);
    assert.deepEqual(pendingFee(w.ctx, FEE_SLOT.creationBurn), { value: 7n, at: BigInt(now(w.ctx) + FEE_NOTICE) });
    warp(w.ctx, FEE_NOTICE);
    await setParam(w.ctx, CONFIG.creationBurn, 7n, [pda.creationBurn(), w.graviMint], w.ctx.payer, true);
    assert.equal(await per(), 7n);
    await setParam(w.ctx, CONFIG.creationBurn, 2n, [pda.creationBurn(), w.graviMint], w.ctx.payer, true);
    assert.equal(await per(), 2n);
    // A cluster that never had the account: its first value is at once.
    w.ctx.svm.setAccount(pda.creationBurn(), { lamports: 0, data: Buffer.alloc(0), owner: SystemProgram.programId, executable: false });
    await setParam(w.ctx, CONFIG.creationBurn, 9n, [pda.creationBurn(), w.graviMint], w.ctx.payer, true);
    assert.equal(await per(), 9n);
  });
});
