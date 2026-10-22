// Fixed after bug bounty reports (October 2026). Mint and redeem price from what the vaults
// hold, not from stored units, so a surplus that reached a vault outside the program (an
// airdrop, a donation, a sleeve a redeemer left behind) can't be minted into at the old price,
// booked with sync_units and redeemed at the new one. Also: the burn's cut of a fee rounds up,
// seeding starts the creator-fee clock, and an asset entry must sit at its own address.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  BN,
  ONE_SHARE,
  SECONDS_PER_YEAR,
  TOKEN_PROGRAM_ID,
  Keypair,
  PublicKey,
  VAULT_ID,
  ata,
  balance,
  expectError,
  fund,
  mintTo,
  pda,
  rpc,
  send,
  supply,
  warp,
} from "./helpers.ts";
import { SEED_SHARES, UNITS_A, UNITS_B, feeShares, fundHolder, mintIx, quads, redeemIx, world, type World } from "./fixtures.ts";

const TEN = 10n * ONE_SHARE;

function syncIx(w: World) {
  return w.ctx.vault.methods
    .syncUnits()
    .accountsPartial({ basket: w.basket, shareMint: w.shareMint })
    .remainingAccounts([w.vaultA, w.vaultB].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false })));
}

/** 10 shares held by `who`, plus 20.0 A that reached the vault without the program. */
async function surplusBasket(protocolFeeBps: number) {
  const w = await world({ protocolFeeBps });
  const who = Keypair.generate();
  const acct = await fundHolder(w, who);
  await rpc(w.ctx, mintIx(w, who, TEN), [who]);
  await mintTo(w.ctx, w.mintA, w.vaultA, 20_000_000n, TOKEN_PROGRAM_ID);
  return { w, who, acct };
}

/** Mint, book the surplus, redeem everything: what the round trip nets a third party. */
async function roundTrip(w: World) {
  const attacker = Keypair.generate();
  const acct = await fundHolder(w, attacker);
  const a0 = balance(w.ctx, acct.a);
  const b0 = balance(w.ctx, acct.b);
  await rpc(w.ctx, mintIx(w, attacker, TEN), [attacker]);
  const held = balance(w.ctx, ata(w.shareMint, attacker.publicKey, w.shareProgram));
  await send(w.ctx, [await syncIx(w).instruction(), await redeemIx(w, attacker, held).instruction()], [attacker]);
  return { dA: balance(w.ctx, acct.a) - a0, dB: balance(w.ctx, acct.b) - b0 };
}

describe("a surplus in a vault belongs to the holders when it arrives", () => {
  it("a third party can't mint into it below its value", async () => {
    const { w } = await surplusBasket(0);
    const { dA, dB } = await roundTrip(w);
    assert.ok(dA <= 0n && dB <= 0n, `the round trip netted A ${dA}, B ${dB}`);
  });

  it("nor with the protocol fee on", async () => {
    const { w } = await surplusBasket(40);
    const { dA, dB } = await roundTrip(w);
    assert.ok(dA <= 0n && dB <= 0n, `the round trip netted A ${dA}, B ${dB}`);
  });

  it("the whole sequence in one transaction gains nothing either", async () => {
    const { w } = await surplusBasket(40);
    const attacker = fund(w.ctx);
    const acct = await fundHolder(w, attacker);
    const a0 = balance(w.ctx, acct.a);
    const fee = (TEN * 40n + 9_999n) / 10_000n;
    await send(w.ctx, [await mintIx(w, attacker, TEN).instruction(), await syncIx(w).instruction(), await redeemIx(w, attacker, TEN - fee).instruction()], [attacker]);
    assert.ok(balance(w.ctx, acct.a) - a0 <= 0n);
  });

  it("a sleeve a redeemer leaves behind goes to the holders who stay, not to the next minter", async () => {
    const w = await world({ protocolFeeBps: 0 });
    const leaver = Keypair.generate();
    await fundHolder(w, leaver);
    await rpc(w.ctx, mintIx(w, leaver, TEN), [leaver]);
    await rpc(w.ctx, redeemIx(w, leaver, TEN, 0b01), [leaver]); // leaves A behind
    const { dA, dB } = await roundTrip(w);
    assert.ok(dA <= 0n && dB <= 0n, `the next minter netted A ${dA}, B ${dB}`);
  });

  it("a third party's round trip doesn't cut what a holder redeems", async () => {
    const control = await surplusBasket(0);
    const c0 = balance(control.w.ctx, control.acct.a);
    await rpc(control.w.ctx, redeemIx(control.w, control.who, TEN), [control.who]);
    const alone = balance(control.w.ctx, control.acct.a) - c0;

    const attacked = await surplusBasket(0);
    await roundTrip(attacked.w);
    const a0 = balance(attacked.w.ctx, attacked.acct.a);
    await rpc(attacked.w.ctx, redeemIx(attacked.w, attacked.who, TEN), [attacked.who]);
    const withAttacker = balance(attacked.w.ctx, attacked.acct.a) - a0;
    // No sync needed for the holder either: the redeem prices from the vault.
    assert.ok(withAttacker >= alone, `holder got ${withAttacker}, ${alone} without the round trip`);
    assert.ok(alone > (UNITS_A * TEN) / ONE_SHARE, "the holder's redemption includes their part of the surplus");
  });
});

describe("the burn gets at least half of every protocol fee", () => {
  it("odd fees included, at the 50% floor and the 0.50% ceiling", async () => {
    const w = await world({ protocolFeeBps: 50, burnShareBps: 5_000 });
    const fs = feeShares(w.shareMint, w.shareProgram);
    const payerShares = ata(w.shareMint, w.creator.publicKey, w.shareProgram);
    for (const shares of [200n, 1000n, 2001n, 4001n, 10001n]) {
      const [b0, t0] = [balance(w.ctx, fs.burnShares), balance(w.ctx, fs.treasuryShares)];
      await rpc(
        w.ctx,
        w.ctx.vault.methods
          .redeemInKind(new BN(shares.toString()), 0)
          .accountsPartial({ payer: w.creator.publicKey, basket: w.basket, shareMint: w.shareMint, payerShares, ...fs, shareTokenProgram: w.shareProgram })
          .remainingAccounts(quads(w, w.creator.publicKey)),
        [w.creator],
      );
      const burn = balance(w.ctx, fs.burnShares) - b0;
      const fee = burn + balance(w.ctx, fs.treasuryShares) - t0;
      assert.equal(fee, (shares * 50n + 9_999n) / 10_000n);
      assert.ok(burn * 2n >= fee, `${shares} shares: burn ${burn} of a ${fee} fee`);
    }
  });
});

describe("the creator fee is owed from the seed, not from creation", () => {
  it("months between creating and seeding aren't charged to anyone", async () => {
    const w = await world({ seed: false, creatorFeeBps: 20 });
    warp(w.ctx, 300 * 86_400);
    const holder = { shares: ata(w.shareMint, w.creator.publicKey, w.shareProgram) };
    await rpc(
      w.ctx,
      w.ctx.vault.methods
        .seedBasket(new BN(SEED_SHARES.toString()), [new BN(UNITS_A.toString()), new BN(UNITS_B.toString())])
        .accountsPartial({ payer: w.creator.publicKey, basket: w.basket, shareMint: w.shareMint, payerShares: holder.shares, ...feeShares(w.shareMint, w.shareProgram), shareTokenProgram: w.shareProgram })
        .remainingAccounts(quads(w, w.creator.publicKey)),
      [w.creator],
    );
    warp(w.ctx, 86_400);
    const before = balance(w.ctx, holder.shares);
    await rpc(
      w.ctx,
      w.ctx.vault.methods
        .accrueFees()
        .accountsPartial({ basket: w.basket, shareMint: w.shareMint, creatorShares: holder.shares, shareTokenProgram: w.shareProgram })
        .remainingAccounts([w.vaultA, w.vaultB].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false }))),
    );
    const minted = balance(w.ctx, holder.shares) - before;
    const oneDay = (SEED_SHARES * 20n * 86_400n) / (10_000n * BigInt(SECONDS_PER_YEAR));
    assert.ok(minted <= oneDay + 1n, `charged ${minted}, one day is ${oneDay}`);
  });
});

describe("an asset entry is only the one at its own address", () => {
  it("a look-alike account can't unpause a basket holding a delisted asset", async () => {
    const w = await world();
    await rpc(
      w.ctx,
      w.ctx.vault.methods
        .setAssetStatus({ delisted: {} })
        .accountsPartial({ signer: w.ctx.payer.publicKey, protocol: pda.protocol(), mint: w.mintB, assetEntry: pda.asset(w.mintB) })
        .remainingAccounts([{ pubkey: w.basket, isSigner: false, isWritable: true }]),
    );
    // On a real cluster nobody else can write an account this program owns; the test
    // environment can, which is the strongest version of the attempt.
    const real: any = await w.ctx.vault.account.assetEntry.fetch(pda.asset(w.mintB));
    const data = await w.ctx.vault.coder.accounts.encode("assetEntry", { ...real, status: { listed: {} } });
    const fake = Keypair.generate().publicKey;
    w.ctx.svm.setAccount(fake, { lamports: 10_000_000, data, owner: VAULT_ID, executable: false });
    await expectError(
      rpc(
        w.ctx,
        w.ctx.vault.methods
          .syncListingStatus()
          .accountsPartial({ basket: w.basket })
          .remainingAccounts([pda.asset(w.mintA), fake].map((pubkey: PublicKey) => ({ pubkey, isSigner: false, isWritable: false }))),
      ),
      "AssetNotListed",
    );
  });
});

// A vault far bigger than its share supply (the supply redeemed down to dust, then a donation)
// puts units per share past u64. Nothing prices from units, so mint and redeem still work, and
// what stores units holds them at u64::MAX instead of failing.
describe("units past u64", () => {
  it("a dust supply and a large donation stop neither redeeming, minting nor syncing", async () => {
    const w = await world();
    const creatorA = ata(w.mintA, w.creator.publicKey);
    await rpc(w.ctx, redeemIx(w, w.creator, SEED_SHARES - 1n), [w.creator]);
    // 2e10 A over 1 raw share is 2e19 units a share: past u64.
    await mintTo(w.ctx, w.mintA, w.vaultA, 20_000_000_000n, TOKEN_PROGRAM_ID);
    await rpc(w.ctx, syncIx(w));
    const b: any = await (w.ctx.vault.account as any).basket.fetch(w.basket);
    assert.equal(BigInt(b.constituents[0].unitsPerShare.toString()), 2n ** 64n - 1n);

    // A new raw share costs a full slice of the vault: no discount for the capped units.
    const minter = Keypair.generate();
    const acct = await fundHolder(w, minter);
    const vaultA = balance(w.ctx, w.vaultA);
    const a0 = balance(w.ctx, acct.a);
    await rpc(w.ctx, mintIx(w, minter, 1n), [minter]);
    assert.equal(a0 - balance(w.ctx, acct.a), vaultA);

    // Both raw shares redeem, half the vault each.
    const c0 = balance(w.ctx, creatorA);
    await rpc(w.ctx, redeemIx(w, w.creator, 1n), [w.creator]);
    assert.equal(balance(w.ctx, creatorA) - c0, vaultA);
    await rpc(w.ctx, redeemIx(w, minter, 1n), [minter]);
    assert.equal(balance(w.ctx, w.vaultA), 0n);
  });
});

describe("a basket with no shares out", () => {
  // Reported through the bug bounty (October 2026): with the supply at 0 after the seed, a mint
  // charged the stored units, not a slice of the vaults, so anyone could mint one raw share into
  // an emptied basket, own all of it and redeem whatever was left behind.
  async function emptied() {
    const w = await world();
    // The only holder leaves everything, giving up sleeve B (as when its token is frozen).
    await rpc(w.ctx, redeemIx(w, w.creator, SEED_SHARES, 0b10), [w.creator]);
    assert.equal(supply(w.ctx, w.shareMint), 0n);
    const left = balance(w.ctx, w.vaultB);
    assert.ok(left > 0n, "sleeve B stays behind");
    return { w, left };
  }

  it("a stranger can't mint into it and take what was left behind", async () => {
    const { w, left } = await emptied();
    const stranger = fund(w.ctx);
    const holder = await fundHolder(w, stranger);
    const before = balance(w.ctx, holder.b);
    await expectError(rpc(w.ctx, mintIx(w, stranger, 1n), [stranger]), "BasketEmpty");
    assert.equal(balance(w.ctx, w.vaultB), left);
    assert.equal(balance(w.ctx, holder.b), before);
  });

  it("its creator can open it again", async () => {
    const { w } = await emptied();
    await rpc(w.ctx, mintIx(w, w.creator, ONE_SHARE), [w.creator]);
    assert.equal(supply(w.ctx, w.shareMint), ONE_SHARE);
    // Once shares are out again, anyone mints at a slice of the vaults, as usual.
    const other = fund(w.ctx);
    await fundHolder(w, other);
    await rpc(w.ctx, mintIx(w, other, ONE_SHARE), [other]);
  });
});
