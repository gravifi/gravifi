// The upgrade that brings `hand_over`, rehearsed: baskets made and changed by the program on mainnet
// (target/deploy/onchain.so: `solana program dump -u m HHmD4ZP5he9GrcmcNrpjahyK1dWh2MhMwmVpiPRJsw3G
// target/deploy/onchain.so`), then this build loaded over it. Skipped without the dump.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { BN, ONE_SHARE, PublicKey, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, VAULT_ID, ata, balance, createAta, createMint, expectError, fund, pda, rpc, warp } from "./helpers.ts";
import { NO_GRAVI, SEED_SHARES, UNITS_A, UNITS_B, feeShares, fundHolder, listAsset, mintIx, quads, redeemIx, world } from "./fixtures.ts";
import type { Keypair } from "@solana/web3.js";

const ONCHAIN = new URL("../target/deploy/onchain.so", import.meta.url).pathname;
const BUILD = new URL("../target/deploy/gravifi_vault.so", import.meta.url).pathname;
const HANDED_TO_AT = 155 + 4 + 2 * 42; // after a two-asset list
const SHARES = 10n * ONE_SHARE;

describe("hand over: upgrading from the program on mainnet", () => {
  it("baskets made before keep working, read as their creator's, and can be handed over", async (t) => {
    if (!existsSync(ONCHAIN)) return t.skip("no dump of the mainnet program at target/deploy/onchain.so");
    const w = await world({ program: ONCHAIN, shareTokenProgram: TOKEN_2022_PROGRAM_ID, seed: false });
    const [alice, bob] = [fund(w.ctx), fund(w.ctx)];
    await fundHolder(w, alice);
    await fundHolder(w, bob);
    const as = (s: Keypair) => ({ authority: s.publicKey, basket: w.basket });

    // On today's program: a third asset added and removed (its bytes stay after the shorter list),
    // a cut set (the void byte to 1), the seed and a buyer.
    const C = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
    await listAsset(w.ctx, C);
    const vaultC = await createAta(w.ctx, C, w.basket, TOKEN_PROGRAM_ID);
    await rpc(w.ctx, w.ctx.vault.methods.addConstituent().accountsPartial({ ...NO_GRAVI, ...as(w.creator), mint: C, assetEntry: pda.asset(C), vault: vaultC }), [w.creator]);
    await rpc(w.ctx, w.ctx.vault.methods.removeConstituent(2).accountsPartial(as(w.creator)), [w.creator]);
    await rpc(w.ctx, w.ctx.vault.methods.setCreatorCut(60).accountsPartial({ ...as(w.creator), notice: null }), [w.creator]);
    await rpc(
      w.ctx,
      w.ctx.vault.methods
        .seedBasket(new BN(SEED_SHARES.toString()), [new BN(UNITS_A.toString()), new BN(UNITS_B.toString())])
        .accountsPartial({ payer: w.creator.publicKey, basket: w.basket, shareMint: w.shareMint, payerShares: ata(w.shareMint, w.creator.publicKey, w.shareProgram), ...feeShares(w.shareMint, w.shareProgram), shareTokenProgram: w.shareProgram })
        .remainingAccounts(quads(w, w.creator.publicKey)),
      [w.creator],
    );
    await rpc(w.ctx, mintIx(w, alice, SHARES), [alice]);
    const raw = Buffer.from(w.ctx.svm.getAccount(w.basket)!.data);
    assert.deepEqual(new PublicKey(raw.subarray(HANDED_TO_AT, HANDED_TO_AT + 32)), C, "the removed asset's mint sits where the manager will be read");
    assert.equal(raw[94], 1, "the cut's void byte, as today's program writes it");
    const size = raw.length;

    // The upgrade.
    w.ctx.svm.addProgramFromFile(VAULT_ID, BUILD);
    warp(w.ctx, 60);

    let b = (await w.ctx.vault.account.basket.fetch(w.basket)) as any;
    assert.deepEqual(b.creator, w.creator.publicKey);
    assert.equal(b.reserved[4] & 2, 0, "not handed over: the leftover bytes mean nothing");
    assert.equal(b.creatorUnclaimed.toString(), "60000000");
    // Still the creator's in every way, and still signing for its vaults.
    await rpc(w.ctx, w.ctx.vault.methods.setTargets([5_000, 5_000]).accountsPartial(as(w.creator)), [w.creator]);
    await expectError(rpc(w.ctx, w.ctx.vault.methods.setTargets([6_000, 4_000]).accountsPartial(as(bob)), [bob]), "NotMandateAuthority");
    await rpc(w.ctx, redeemIx(w, alice, SHARES / 2n), [alice]);

    // Handed over after the upgrade.
    await rpc(w.ctx, w.ctx.vault.methods.handOver(bob.publicKey).accountsPartial({ manager: w.creator.publicKey, basket: w.basket }), [w.creator]);
    b = (await w.ctx.vault.account.basket.fetch(w.basket)) as any;
    assert.equal(b.reserved[4], 3, "the void bit kept, the handover bit set");
    assert.deepEqual(b.handedTo, bob.publicKey);
    assert.equal(w.ctx.svm.getAccount(w.basket)!.data.length, size, "no realloc");
    await expectError(rpc(w.ctx, w.ctx.vault.methods.setTargets([6_000, 4_000]).accountsPartial(as(w.creator)), [w.creator]), "NotMandateAuthority");
    await rpc(w.ctx, w.ctx.vault.methods.setTargets([6_000, 4_000]).accountsPartial(as(bob)), [bob]);
    // What the cut earned before the handover is the new manager's to claim.
    const bobShares = ata(w.shareMint, bob.publicKey, w.shareProgram);
    const before = balance(w.ctx, bobShares);
    await rpc(
      w.ctx,
      w.ctx.vault.methods.claimCreatorCut().accountsPartial({ basket: w.basket, protocol: pda.protocol(), shareMint: w.shareMint, treasuryShares: feeShares(w.shareMint, w.shareProgram).treasuryShares, creatorShares: bobShares, shareTokenProgram: w.shareProgram }),
    );
    assert.equal(balance(w.ctx, bobShares) - before, 60_000_000n);
    await rpc(w.ctx, redeemIx(w, alice, SHARES / 4n), [alice]);
  });
});
