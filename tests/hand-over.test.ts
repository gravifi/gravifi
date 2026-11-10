// Handing a basket over: its creator gives every creator power and payout to another wallet
// (`hand_over`). `creator` stays the basket PDA's seed, so the basket still signs for its vaults and
// its share mint; the manager (`Basket::manager`) is who every creator check and payout reads.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { SystemProgram, SYSVAR_INSTRUCTIONS_PUBKEY, type Keypair } from "@solana/web3.js";
import { existsSync } from "node:fs";
import {
  BN,
  ONE_SHARE,
  PublicKey,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  ata,
  balance,
  createAta,
  createMint,
  expectError,
  fund,
  pda,
  rpc,
  supply,
  warp,
} from "./helpers.ts";
import { NO_GRAVI, SEED_SHARES, UNITS_A, UNITS_B, feeShares, fundHolder, listAsset, mintIx, quads, redeemIx, world, type World, type WorldOpts } from "./fixtures.ts";

const DAY = 86_400;
const SHARES = 10n * ONE_SHARE;
/** Where the Basket account keeps things, past its 8-byte discriminator: `reserved` (byte 4 holds
 *  the HANDED_OVER bit, 2), the asset list's length, its first element, and each element's size. */
const RESERVED4_AT = 94;
const LIST_AT = 155;
const CONSTITUENT = 42;
/** Every basket's account size, before and after this change. */
const BASKET_SIZE = LIST_AT + 4 + 16 * CONSTITUENT;

type Who = Keypair;

/** A world whose creator has handed the basket to `bob`, with `alice` holding shares. */
async function handed(opts: WorldOpts = {}) {
  const w = await world({ shareTokenProgram: TOKEN_2022_PROGRAM_ID, ...opts });
  const bob = fund(w.ctx);
  const alice = fund(w.ctx);
  await fundHolder(w, bob);
  await fundHolder(w, alice);
  await handOver(w, bob.publicKey);
  return { ...w, bob, alice };
}
type H = Awaited<ReturnType<typeof handed>>;

const handOver = (w: World, to: PublicKey, signer: Who = w.creator) =>
  rpc(w.ctx, w.ctx.vault.methods.handOver(to).accountsPartial({ manager: signer.publicKey, basket: w.basket }), [signer]);

const fetch = (w: World) => w.ctx.vault.account.basket.fetch(w.basket) as Promise<any>;

/** The basket's manager, read as the app reads it. */
async function managerOf(w: World): Promise<PublicKey> {
  const b = await fetch(w);
  return b.reserved[4] & 2 ? b.handedTo : b.creator;
}

const sharesOf = (w: World, who: PublicKey) => ata(w.shareMint, who, w.shareProgram);

const setTargets = (w: World, s: Who, bps: number[]) =>
  rpc(w.ctx, w.ctx.vault.methods.setTargets(bps).accountsPartial({ authority: s.publicKey, basket: w.basket }), [s]);

const setPolicy = (w: World, s: Who) =>
  rpc(w.ctx, w.ctx.vault.methods.setPolicy({ hybrid: {} }, new BN(7 * DAY), 600, 10).accountsPartial({ authority: s.publicKey, basket: w.basket }), [s]);

const split = (w: World, s: Who) =>
  rpc(w.ctx, w.ctx.vault.methods.split(2, 1).accountsPartial({ authority: s.publicKey, basket: w.basket, shareMint: w.shareMint, tokenProgram: TOKEN_2022_PROGRAM_ID }), [s]);

const setStops = (w: World, s: Who) =>
  rpc(w.ctx, w.ctx.vault.methods.setStops([]).accountsPartial({ creator: s.publicKey, basket: w.basket, stopLoss: pda.stop(w.basket) }), [s]);

const closeStops = (w: World, s: Who) =>
  rpc(w.ctx, w.ctx.vault.methods.closeStops().accountsPartial({ creator: s.publicKey, basket: w.basket, stopLoss: pda.stop(w.basket) }), [s]);

const setCut = (w: World, s: Who, bps: number) =>
  rpc(w.ctx, w.ctx.vault.methods.setCreatorCut(bps).accountsPartial({ authority: s.publicKey, basket: w.basket, notice: null }), [s]);

const proposeCut = (w: World, s: Who, bps: number) =>
  rpc(w.ctx, w.ctx.vault.methods.proposeCutRise(bps).accountsPartial({ authority: s.publicKey, basket: w.basket, notice: pda.cutNotice(w.basket) }), [s]);

const applyCut = (w: World, creator: PublicKey) =>
  rpc(w.ctx, w.ctx.vault.methods.applyCutRise().accountsPartial({ basket: w.basket, notice: pda.cutNotice(w.basket), creator }));

const claim = (w: World, to: PublicKey) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.claimCreatorCut().accountsPartial({ ext: null,
      basket: w.basket,
      protocol: pda.protocol(),
      shareMint: w.shareMint,
      treasuryShares: feeShares(w.shareMint, w.shareProgram).treasuryShares,
      creatorShares: sharesOf(w, to),
      shareTokenProgram: w.shareProgram,
    }),
  );

const accrue = (w: World, to: PublicKey) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods
      .accrueFees()
      .accountsPartial({ basket: w.basket, shareMint: w.shareMint, creatorShares: sharesOf(w, to), shareTokenProgram: w.shareProgram })
      .remainingAccounts([w.vaultA, w.vaultB].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false }))),
  );

const allow = (w: World, s: Who, wallet: PublicKey) =>
  rpc(w.ctx, w.ctx.vault.methods.allowWallet().accountsPartial({ creator: s.publicKey, basket: w.basket, wallet, allowEntry: pda.allow(w.basket, wallet) }), [s]);

const disallow = (w: World, s: Who, wallet: PublicKey) =>
  rpc(w.ctx, w.ctx.vault.methods.disallowWallet().accountsPartial({ creator: s.publicKey, basket: w.basket, wallet, allowEntry: pda.allow(w.basket, wallet) }), [s]);

const seed = (w: World, s: Who) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods
      .seedBasket(new BN(SEED_SHARES.toString()), [new BN(UNITS_A.toString()), new BN(UNITS_B.toString())])
      .accountsPartial({ payer: s.publicKey, basket: w.basket, shareMint: w.shareMint, payerShares: sharesOf(w, s.publicKey), ...feeShares(w.shareMint, w.shareProgram), shareTokenProgram: w.shareProgram })
      .remainingAccounts(quads(w, s.publicKey)),
    [s],
  );

/** A third listed asset, with the basket's vault for it. */
async function assetC(w: World) {
  const mint = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
  await listAsset(w.ctx, mint);
  const vault = await createAta(w.ctx, mint, w.basket, TOKEN_PROGRAM_ID);
  return { mint, vault };
}

const addAsset = (w: World, s: Who, c: { mint: PublicKey; vault: PublicKey }) =>
  rpc(w.ctx, w.ctx.vault.methods.addConstituent().accountsPartial({ ...NO_GRAVI, authority: s.publicKey, basket: w.basket, mint: c.mint, assetEntry: pda.asset(c.mint), vault: c.vault }), [s]);

const removeAsset = (w: World, s: Who, index: number) =>
  rpc(w.ctx, w.ctx.vault.methods.removeConstituent(index).accountsPartial({ authority: s.publicKey, basket: w.basket }), [s]);

const MPL = new PublicKey("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
const MPL_SO = new URL("../target/deploy/mpl_token_metadata.so", import.meta.url).pathname;
const metadataPda = (mint: PublicKey) => PublicKey.findProgramAddressSync([Buffer.from("metadata"), MPL.toBuffer(), mint.toBuffer()], MPL)[0];
const setMetadata = (w: World, s: Who) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.setShareMetadata("Handed", "HND", "https://app.gravifi.xyz/api/meta/x").accountsPartial({
      authority: s.publicKey,
      payer: s.publicKey,
      basket: w.basket,
      shareMint: w.shareMint,
      metadata: metadataPda(w.shareMint),
      tokenProgram: w.shareProgram,
      tokenMetadataProgram: MPL,
      systemProgram: SystemProgram.programId,
      sysvarInstructions: SYSVAR_INSTRUCTIONS_PUBKEY,
    }),
    [s],
  );

describe("hand over: the handover", () => {
  it("moves the basket to the new manager; the creator, the address and the account size stay", async () => {
    const w = await world();
    const before = w.ctx.svm.getAccount(w.basket)!.data.length;
    assert.equal(before, BASKET_SIZE, "a new basket is the size every basket on chain already is");
    assert.deepEqual(await managerOf(w), w.creator.publicKey, "the creator manages it until it's handed over");
    const bob = fund(w.ctx);
    await handOver(w, bob.publicKey);
    const b = await fetch(w);
    assert.deepEqual(b.creator, w.creator.publicKey);
    assert.deepEqual(b.handedTo, bob.publicKey);
    assert.equal(b.reserved[4] & 2, 2);
    assert.deepEqual(await managerOf(w), bob.publicKey);
    assert.deepEqual(pda.basket(w.creator.publicKey, w.basketId), w.basket);
    assert.equal(w.ctx.svm.getAccount(w.basket)!.data.length, before);
  });

  it("only the manager hands it over, never to nobody or to itself", async () => {
    const w = await world();
    const [bob, carol] = [fund(w.ctx), fund(w.ctx)];
    await expectError(handOver(w, carol.publicKey, bob), "NotMandateAuthority");
    await expectError(handOver(w, PublicKey.default), "BadNewManager");
    await expectError(handOver(w, w.creator.publicKey), "BadNewManager");
    await handOver(w, bob.publicKey);
    await expectError(handOver(w, carol.publicKey), "NotMandateAuthority");
    await expectError(handOver(w, bob.publicKey, bob), "BadNewManager");
    // The new manager can pass it on, and back to its creator, which clears it.
    await handOver(w, carol.publicKey, bob);
    assert.deepEqual(await managerOf(w), carol.publicKey);
    await handOver(w, w.creator.publicKey, carol);
    const b = await fetch(w);
    assert.equal(b.reserved[4] & 2, 0);
    assert.deepEqual(b.handedTo, PublicKey.default);
    await setTargets(w, w.creator, [5_000, 5_000]);
  });

  it("an immutable basket can be handed over too: its manager owns its cut and metadata", async () => {
    const w = await world({ control: "immutable", seed: false });
    const bob = fund(w.ctx);
    await handOver(w, bob.publicKey);
    await expectError(setCut(w, w.creator, 30), "NotMandateAuthority");
    await setCut(w, bob, 30);
    assert.equal((await fetch(w)).creatorCutBps, 30);
  });

  it("bytes left after the asset list by an older basket mean nothing until a handover sets the bit", async () => {
    const w = await world();
    const bob = fund(w.ctx);
    // A removed asset leaves its bytes after the shorter list; say they spell bob's key.
    const acc = w.ctx.svm.getAccount(w.basket)!;
    const data = Buffer.from(acc.data);
    bob.publicKey.toBuffer().copy(data, LIST_AT + 4 + 2 * CONSTITUENT);
    w.ctx.svm.setAccount(w.basket, { ...acc, data });
    assert.deepEqual(await managerOf(w), w.creator.publicKey);
    await expectError(setTargets(w, bob, [5_000, 5_000]), "NotMandateAuthority");
    await setTargets(w, w.creator, [5_000, 5_000]);
    // The bit is what makes them the manager (this checks the layout the app reads, too).
    const acc2 = w.ctx.svm.getAccount(w.basket)!;
    const data2 = Buffer.from(acc2.data);
    data2[RESERVED4_AT] |= 2;
    w.ctx.svm.setAccount(w.basket, { ...acc2, data: data2 });
    await setTargets(w, bob, [6_000, 4_000]);
    await expectError(setTargets(w, w.creator, [5_000, 5_000]), "NotMandateAuthority");
  });

  it("the manager moves with the asset list as it grows and shrinks", async () => {
    const w = await handed({ seed: false });
    const c = await assetC(w);
    await expectError(addAsset(w, w.creator, c), "NotMandateAuthority");
    await addAsset(w, w.bob, c);
    assert.equal((await fetch(w)).constituentCount, 3);
    assert.deepEqual(await managerOf(w), w.bob.publicKey);
    await expectError(removeAsset(w, w.creator, 2), "NotMandateAuthority");
    await removeAsset(w, w.bob, 2);
    assert.equal((await fetch(w)).constituentCount, 2);
    assert.deepEqual(await managerOf(w), w.bob.publicKey);
    await setTargets(w, w.bob, [5_000, 5_000]);
  });
});

describe("hand over: the old creator loses every creator action, the new manager gains it", () => {
  it("targets, policy and splits", async () => {
    const w = await handed();
    await expectError(setTargets(w, w.creator, [5_000, 5_000]), "NotMandateAuthority");
    await expectError(setPolicy(w, w.creator), "NotMandateAuthority");
    await expectError(split(w, w.creator), "NotMandateAuthority");
    await setTargets(w, w.bob, [5_000, 5_000]);
    await setPolicy(w, w.bob);
    await split(w, w.bob);
    const b = await fetch(w);
    assert.deepEqual(b.constituents.slice(0, 2).map((c: any) => c.targetBps), [5_000, 5_000]);
    assert.equal(b.mgmtFeeBps, 10);
  });

  it("stops: setting them and closing them, the rent back to the manager", async () => {
    const w = await handed();
    await expectError(setStops(w, w.creator), "NotMandateAuthority");
    await setStops(w, w.bob);
    await expectError(closeStops(w, w.creator), "NotMandateAuthority");
    const before = w.ctx.svm.getBalance(w.bob.publicKey)!;
    await closeStops(w, w.bob);
    assert.ok(w.ctx.svm.getBalance(w.bob.publicKey)! > before, "the stop account's rent went to the manager");
    assert.equal(w.ctx.svm.getAccount(pda.stop(w.basket))?.data.length ?? 0, 0);
  });

  it("the cut: setting it, asking for a rise, applying it with the rent to the manager", async () => {
    const w = await handed({ seed: false });
    await expectError(setCut(w, w.creator, 20), "NotMandateAuthority");
    await setCut(w, w.bob, 20);
    await seed(w, w.bob);
    await expectError(proposeCut(w, w.creator, 40), "NotMandateAuthority");
    await proposeCut(w, w.bob, 40);
    // Clearing the void bit kept the handover bit.
    assert.deepEqual(await managerOf(w), w.bob.publicKey);
    warp(w.ctx, DAY + 1);
    await expectError(applyCut(w, w.creator.publicKey), "NotMandateAuthority");
    const before = w.ctx.svm.getBalance(w.bob.publicKey)!;
    await applyCut(w, w.bob.publicKey);
    assert.ok(w.ctx.svm.getBalance(w.bob.publicKey)! > before, "the notice's rent went to the manager");
    assert.equal((await fetch(w)).creatorCutBps, 40);
    // Setting it again sets the void bit, beside the handover bit.
    await setCut(w, w.bob, 10);
    assert.deepEqual(await managerOf(w), w.bob.publicKey);
    assert.equal((await fetch(w)).reserved[4], 3);
  });

  it("the cut is paid to the manager: others' mints owe it, the manager's own don't, and only the manager's account takes it", async () => {
    const w = await handed({ seed: false });
    await setCut(w, w.bob, 60);
    await seed(w, w.bob);
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), "0", "the manager's seed pays no cut");
    await rpc(w.ctx, mintIx(w, w.bob, SHARES), [w.bob]);
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), "0", "nor the manager's own mint");
    // The old creator is just a buyer now.
    await rpc(w.ctx, mintIx(w, w.creator, SHARES), [w.creator]);
    await rpc(w.ctx, mintIx(w, w.alice, SHARES), [w.alice]);
    const owed = BigInt((await fetch(w)).creatorUnclaimed.toString());
    assert.equal(owed, 2n * 60_000_000n);
    await expectError(claim(w, w.creator.publicKey), "ConstraintTokenOwner");
    const before = balance(w.ctx, sharesOf(w, w.bob.publicKey));
    await claim(w, w.bob.publicKey);
    assert.equal(balance(w.ctx, sharesOf(w, w.bob.publicKey)) - before, owed);
    assert.equal((await fetch(w)).creatorUnclaimed.toString(), "0");
  });

  it("the yearly fee streams to the manager, minted by the basket's own signature", async () => {
    const w = await handed();
    warp(w.ctx, 30 * DAY);
    await expectError(accrue(w, w.creator.publicKey), "ConstraintTokenOwner");
    const [held, sup] = [balance(w.ctx, sharesOf(w, w.bob.publicKey)), supply(w.ctx, w.shareMint)];
    await accrue(w, w.bob.publicKey);
    const got = balance(w.ctx, sharesOf(w, w.bob.publicKey)) - held;
    assert.ok(got > 0n, "the fee was paid");
    assert.equal(supply(w.ctx, w.shareMint) - sup, got);
  });

  it("the allowlist: approving and removing wallets, the rent back to the manager", async () => {
    const w = await handed({ access: "allowlist" });
    await expectError(allow(w, w.creator, w.alice.publicKey), "NotMandateAuthority");
    await allow(w, w.bob, w.alice.publicKey);
    await rpc(w.ctx, mintIx(w, w.alice, SHARES, pda.allow(w.basket, w.alice.publicKey)), [w.alice]);
    await expectError(disallow(w, w.creator, w.alice.publicKey), "NotMandateAuthority");
    const before = w.ctx.svm.getBalance(w.bob.publicKey)!;
    await disallow(w, w.bob, w.alice.publicKey);
    assert.ok(w.ctx.svm.getBalance(w.bob.publicKey)! > before);
  });

  it("a private basket: only the manager mints into it", async () => {
    const w = await handed({ access: "private" });
    await expectError(rpc(w.ctx, mintIx(w, w.creator, SHARES), [w.creator]), "MintNotPermitted");
    await rpc(w.ctx, mintIx(w, w.bob, SHARES), [w.bob]);
    assert.equal(balance(w.ctx, sharesOf(w, w.bob.publicKey)), SHARES);
  });

  it("the seed, and the first mint into an emptied basket, are the manager's", async () => {
    const w = await handed({ seed: false });
    await expectError(seed(w, w.creator), "MintNotPermitted");
    await seed(w, w.bob);
    // Everything out: the vaults are left to whoever opens it again, which only the manager may.
    await rpc(w.ctx, redeemIx(w, w.bob, SEED_SHARES), [w.bob]);
    assert.equal(supply(w.ctx, w.shareMint), 0n);
    await expectError(rpc(w.ctx, mintIx(w, w.alice, SHARES), [w.alice]), "BasketEmpty");
    await expectError(rpc(w.ctx, mintIx(w, w.creator, SHARES), [w.creator]), "BasketEmpty");
    await rpc(w.ctx, mintIx(w, w.bob, SHARES), [w.bob]);
  });

  it("the share's name and logo in wallets, written with the basket's signature", async (t) => {
    if (!existsSync(MPL_SO)) return t.skip("Metaplex isn't in target/deploy");
    const w = await handed();
    w.ctx.svm.addProgramFromFile(MPL, MPL_SO);
    await expectError(setMetadata(w, w.creator), "NotMetadataAuthority");
    await setMetadata(w, w.bob);
    assert.ok((w.ctx.svm.getAccount(metadataPda(w.shareMint))?.data.length ?? 0) > 0);
  });
});

describe("hand over: the basket still signs for its vaults", () => {
  it("mints and redeems move tokens in and out of the vaults as before", async () => {
    const w = await handed();
    await rpc(w.ctx, mintIx(w, w.alice, SHARES), [w.alice]);
    const [a, b] = [balance(w.ctx, w.vaultA), balance(w.ctx, w.vaultB)];
    await rpc(w.ctx, redeemIx(w, w.alice, SHARES), [w.alice]);
    assert.equal(a - balance(w.ctx, w.vaultA), (UNITS_A * SHARES) / ONE_SHARE);
    assert.ok(b - balance(w.ctx, w.vaultB) > 0n);
    assert.equal(balance(w.ctx, sharesOf(w, w.alice.publicKey)), 0n);
    // The creator's own seed shares, too: they're just a holder now.
    await rpc(w.ctx, redeemIx(w, w.creator, SEED_SHARES), [w.creator]);
  });
});
