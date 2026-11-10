// A part (a mixed basket's Robinhood Chain side, bridged to Solana as an OFT token) proving itself
// on chain (`verify_part`), and smart baskets with no protocol ceilings (the team, 8 Nov 2026: "No, I
// want them gone"). The OFT program's accounts are written by hand (tests/_parts.ts).
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { PublicKey, SystemProgram, TOKEN_PROGRAM_ID, createMint, expectError, fund, pda, rpc, setup, warp } from "./helpers.ts";
import { initProtocol, listAsset, setRoleIx, ROLE, ROLE_NOTICE } from "./fixtures.ts";
import { DAY, EID, GUARD, OFT, enable, hasRecord, named, peerOf, setTargets, targets, verifyPart, wirePart, world } from "./_parts.ts";

describe("parts: verify_part", () => {
  it("gives a part its Verified record once its mint, store, admin and peer prove it; anyone pays", async () => {
    const w = await world();
    const { store, peer } = await wirePart(w.ctx, w.P, w.admin.publicKey);
    const anyone = fund(w.ctx);
    // Nothing named yet: nothing proves a part.
    await expectError(verifyPart(w.ctx, w.P, store, peer, anyone), "NotAPart");
    // Named, but a key named waits its day.
    await rpc(w.ctx, setRoleIx(w.ctx, ROLE.partAdmin, w.admin.publicKey));
    await rpc(w.ctx, setRoleIx(w.ctx, ROLE.partOft, OFT));
    await expectError(verifyPart(w.ctx, w.P, store, peer, anyone), "NotAPart");
    warp(w.ctx, ROLE_NOTICE);
    await verifyPart(w.ctx, w.P, store, peer, anyone);
    assert.ok(hasRecord(w, w.P));
    await expectError(verifyPart(w.ctx, w.P, store, peer, anyone), "AlreadyVerified");
    // The authority can still take it away (and clearing the admin stops it coming back).
    await rpc(w.ctx, w.ctx.vault.methods.setVerified(false).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol(), mint: w.P, verified: pda.verified(w.P) }));
    assert.ok(!hasRecord(w, w.P));
    await rpc(w.ctx, setRoleIx(w.ctx, ROLE.partAdmin, PublicKey.default));
    await expectError(verifyPart(w.ctx, w.P, store, peer, anyone), "NotAPart");
  });

  it("refuses anything that isn't one of ours", async () => {
    const w = await world();
    await named(w);
    const payer = fund(w.ctx);
    const fresh = async () => {
      const m = await createMint(w.ctx, 9, TOKEN_PROGRAM_ID);
      await listAsset(w.ctx, m);
      return m;
    };
    const cases: [string, () => Promise<{ mint: PublicKey; store: PublicKey; peer: PublicKey; eid?: number }>][] = [
      ["a store another key runs", async () => { const mint = await fresh(); return { mint, ...(await wirePart(w.ctx, mint, fund(w.ctx).publicKey)) }; }],
      ["a store of another program", async () => { const mint = await fresh(); return { mint, ...(await wirePart(w.ctx, mint, w.admin.publicKey, { program: PublicKey.unique() })) }; }],
      ["a store for another mint", async () => { const mint = await fresh(); return { mint, ...(await wirePart(w.ctx, mint, w.admin.publicKey, { storeMint: w.A })) }; }],
      ["a mint the store doesn't mint", async () => { const mint = await fresh(); return { mint, ...(await wirePart(w.ctx, mint, w.admin.publicKey, { keepAuthority: true })) }; }],
      ["no peer (the admin never wired it)", async () => { const mint = await fresh(); return { mint, ...(await wirePart(w.ctx, mint, w.admin.publicKey, { noPeer: true })) }; }],
      ["a peer with no address", async () => { const mint = await fresh(); return { mint, ...(await wirePart(w.ctx, mint, w.admin.publicKey, { peerAddress: Buffer.alloc(32) })) }; }],
      ["the peer of another eid, named as this one's", async () => {
        const mint = await fresh();
        const { store } = await wirePart(w.ctx, mint, w.admin.publicKey, { peerEid: 40_168 });
        return { mint, store, peer: peerOf(store, 40_168), eid: EID };
      }],
      ["a store passed as the peer", async () => { const mint = await fresh(); const { store } = await wirePart(w.ctx, mint, w.admin.publicKey); return { mint, store, peer: store }; }],
    ];
    for (const [why, make] of cases) {
      const c = await make();
      await expectError(verifyPart(w.ctx, c.mint, c.store, c.peer, payer, c.eid), "NotAPart").catch((e) => assert.fail(`${why}: ${e}`));
    }
    // A wrong record address.
    const { store, peer } = await wirePart(w.ctx, w.P, w.admin.publicKey);
    await expectError(
      rpc(w.ctx, w.ctx.vault.methods.verifyPart(EID).accountsPartial({ payer: payer.publicKey, parts: pda.parts(), mint: w.P, oftStore: store, peer, verified: pda.verified(w.A), systemProgram: SystemProgram.programId }), [payer]),
      "BadVerifiedRecord",
    );
  });

  it("a verified part weighs up to the weight cap in a verified-only smart basket", async () => {
    const w = await world([2_000, 2_000, 2_000, 4_000]);
    const guard = { ...GUARD, maxWeightBps: 4_500, maxUnverifiedBps: 0 };
    // Without its record, the part is held to the Unverified cap: 0 here.
    await expectError(enable(w, guard), "WeightAboveCap");
    await named(w);
    const { store, peer } = await wirePart(w.ctx, w.P, w.admin.publicKey);
    await verifyPart(w.ctx, w.P, store, peer, w.creator);
    await enable(w, guard);
    await setTargets(w, [1_500, 2_000, 2_000, 4_500]);
    assert.deepEqual(await targets(w), [1_500, 2_000, 2_000, 4_500]);
  });

  it("the parts' keys wait a day each, in their own account", async () => {
    const ctx = setup();
    await initProtocol(ctx, 40);
    const admin = PublicKey.unique();
    await rpc(ctx, setRoleIx(ctx, ROLE.partAdmin, admin));
    const d = Buffer.from(ctx.svm.getAccount(pda.parts())!.data);
    assert.equal(d.length, 297);
    assert.deepEqual(new PublicKey(d.subarray(9, 41)), PublicKey.default, "not acting yet");
    assert.deepEqual(new PublicKey(d.subarray(73, 105)), admin, "waiting");
    // set_role names the parts' account and nothing else for these roles.
    await expectError(rpc(ctx, ctx.vault.methods.setRole(ROLE.partOft, OFT).accountsPartial({ authority: ctx.payer.publicKey, protocol: pda.protocol() }).remainingAccounts([{ pubkey: pda.keepers(), isSigner: false, isWritable: true }])), "BadRole");
  });
});

describe("smart baskets: no protocol ceilings", () => {
  it("takes guardrails past the old 40% a day, 100% a week, 35% an asset and 30% unverified", async () => {
    const w = await world([6_000, 1_000, 1_000, 2_000]);
    const wide = { maxDayBps: 8_000, maxWeekBps: 30_000, maxWeightBps: 7_000, maxUnverifiedBps: 6_000, minAssets: 2, maxAssets: 4, stopBps: 0, stopTrailing: false };
    await enable(w, wide);
    // 60 points in one review: A 60% -> 10%, C 10% -> 60% (C is Verified, under its 70%).
    await setTargets(w, [1_000, 1_000, 6_000, 2_000]);
    // The creator's own day cap still holds: 80% a day, 60 used.
    await expectError(setTargets(w, [1_000, 1_000, 2_500, 5_500]), "TurnoverDayCap");
    warp(w.ctx, DAY);
    // The part (no record) up to its creator's 60% Unverified cap, no further.
    await expectError(setTargets(w, [1_000, 1_000, 1_500, 6_500]), "WeightAboveCap");
    await setTargets(w, [1_000, 1_000, 2_000, 6_000]);
    assert.deepEqual(await targets(w), [1_000, 1_000, 2_000, 6_000]);
  });
});
