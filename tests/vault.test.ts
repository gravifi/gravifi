import { describe, test } from "node:test";
import assert from "node:assert/strict";
import { Keypair, PublicKey } from "@solana/web3.js";
import { createTransferCheckedInstruction } from "@solana/spl-token";
import {
  BN,
  ONE_SHARE,
  SECONDS_PER_YEAR,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  ata,
  balance,
  createAta,
  createMint,
  expectError,
  fakeProgramData,
  fund,
  mintTo,
  now,
  pda,
  rpc,
  send,
  setup,
  supply,
  warp,
} from "./helpers.ts";
import { NO_GRAVI,
  SEED_SHARES,
  UNITS_A,
  UNITS_B,
  createFeeAccounts,
  feeShares,
  fundHolder,
  initProtocol,
  listAsset,
  mintIx,
  quads,
  redeemIx,
  world,
  type World,
} from "./fixtures.ts";

const ceilDiv = (a: bigint, b: bigint) => (a + b - 1n) / b;

function feeAccounts(w: World) {
  return { ...feeShares(w.shareMint), creatorShares: ata(w.shareMint, w.creator.publicKey) };
}

function accrueIx(w: World, accts: { creatorShares: PublicKey }) {
  return w.ctx.vault.methods
    .accrueFees()
    .accountsPartial({
      basket: w.basket,
      shareMint: w.shareMint,
      creatorShares: accts.creatorShares,
      shareTokenProgram: TOKEN_PROGRAM_ID,
    })
    .remainingAccounts([w.vaultA, w.vaultB].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false })));
}

async function units(w: World): Promise<[bigint, bigint]> {
  const b: any = await w.ctx.vault.account.basket.fetch(w.basket);
  return [BigInt(b.constituents[0].unitsPerShare.toString()), BigInt(b.constituents[1].unitsPerShare.toString())];
}

/** Overwrites a token account's amount in place, standing in for an issuer clawback. */
function setTokenAmount(w: World, account: PublicKey, amount: bigint) {
  const acc = w.ctx.svm.getAccount(account)!;
  const data = Buffer.from(acc.data);
  data.writeBigUInt64LE(amount, 64);
  w.ctx.svm.setAccount(account, { ...acc, data });
}

describe("protocol setup", () => {
  test("only the program's upgrade authority can initialise the protocol", async () => {
    const ctx = setup();
    const outsider = Keypair.generate();
    const programData = fakeProgramData(ctx, outsider.publicKey);
    await expectError(
      rpc(
        ctx,
        ctx.vault.methods.initProtocol(PublicKey.default, 10, 5_000).accountsPartial({
          authority: ctx.payer.publicKey,
          protocol: pda.protocol(),
          programData,
        }),
      ),
      "NotProtocolAuthority",
    );
  });

  test("the protocol fee cannot start above its 0.20% ceiling", async () => {
    const ctx = setup();
    await expectError(initProtocol(ctx, 51, 5_000), "FeeTooHigh");
  });
});

describe("asset registry", () => {
  test("only the protocol authority can list an asset", async () => {
    const w = await world();
    const outsider = fund(w.ctx);
    const mint = await createMint(w.ctx, 6);
    await expectError(
      rpc(
        w.ctx,
        w.ctx.vault.methods.registerAsset().accountsPartial({
          payer: outsider.publicKey,
          authority: outsider.publicKey,
          protocol: pda.protocol(),
          mint,
          assetEntry: pda.asset(mint),
        }),
        [outsider],
      ),
      "NotProtocolAuthority",
    );
  });

  test("a mint with an enabled transfer hook is refused", async () => {
    const ctx = setup();
    await initProtocol(ctx);
    const hooked = await createMint(ctx, 6, TOKEN_2022_PROGRAM_ID, Keypair.generate().publicKey);
    await expectError(listAsset(ctx, hooked), "TransferHookUnsupported");
  });

  test("a basket cannot include an unlisted asset", async () => {
    const w = await world();
    const unlisted = await createMint(w.ctx, 6);
    const id = new Uint8Array(16).fill(3);
    const basket = pda.basket(w.creator.publicKey, id);
    await expectError(
      rpc(
        w.ctx,
        w.ctx.vault.methods
          .initializeBasket(basketArgs(id, [w.mintA, unlisted], [5_000, 5_000]))
          .accountsPartial({ creator: w.creator.publicKey, basket, shareMint: pda.share(basket), shareTokenProgram: TOKEN_PROGRAM_ID })
          .remainingAccounts(entries([w.mintA, unlisted])),
        [w.creator],
      ),
      "AssetNotListed",
    );
  });

  test("the guardian can delist but never list", async () => {
    const w = await world();
    const guardian = fund(w.ctx);
    await rpc(w.ctx, w.ctx.vault.methods.setGuardian(guardian.publicKey).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol() }));

    const statusIx = (status: object) =>
      w.ctx.vault.methods.setAssetStatus(status).accountsPartial({
        signer: guardian.publicKey,
        protocol: pda.protocol(),
        mint: w.mintA,
        assetEntry: pda.asset(w.mintA),
      });

    await rpc(w.ctx, statusIx({ delisted: {} }), [guardian]);
    const entry: any = await w.ctx.vault.account.assetEntry.fetch(pda.asset(w.mintA));
    assert.ok("delisted" in entry.status);

    await expectError(rpc(w.ctx, statusIx({ listed: {} }), [guardian]), "NotProtocolAuthority");
  });

  test("a delisting pauses minting but never redemption", async () => {
    const w = await world();
    await rpc(
      w.ctx,
      w.ctx.vault.methods.setAssetStatus({ delisted: {} }).accountsPartial({
        signer: w.ctx.payer.publicKey,
        protocol: pda.protocol(),
        mint: w.mintB,
        assetEntry: pda.asset(w.mintB),
      }),
    );
    // Anyone can sync; here a random account does it.
    await rpc(
      w.ctx,
      w.ctx.vault.methods
        .syncListingStatus()
        .accountsPartial({ basket: w.basket })
        .remainingAccounts(entries([w.mintA, w.mintB]).filter((_, i) => i % 2 === 1)),
    );

    await expectError(rpc(w.ctx, mintIx(w, w.creator, ONE_SHARE), [w.creator]), "MintPaused");
    await rpc(w.ctx, redeemIx(w, w.creator, ONE_SHARE), [w.creator]);
    assert.equal(supply(w.ctx, w.shareMint), SEED_SHARES - ONE_SHARE);
  });

  const delistIx = (w: World, mint: PublicKey, baskets: PublicKey[]) =>
    w.ctx.vault.methods
      .setAssetStatus({ delisted: {} })
      .accountsPartial({ signer: w.ctx.payer.publicKey, protocol: pda.protocol(), mint, assetEntry: pda.asset(mint) })
      .remainingAccounts(baskets.map((pubkey) => ({ pubkey, isSigner: false, isWritable: true })));

  test("a delisting pauses minting into the baskets passed with it, in the same instruction", async () => {
    const w = await world();
    await rpc(w.ctx, delistIx(w, w.mintB, [w.basket]));
    // No sync in between: there's no window in which new money still goes in.
    await expectError(rpc(w.ctx, mintIx(w, w.creator, ONE_SHARE), [w.creator]), "MintPaused");
    await rpc(w.ctx, redeemIx(w, w.creator, ONE_SHARE), [w.creator]);
    // A repeat delisting still pauses a basket that was missed the first time.
    await rpc(w.ctx, delistIx(w, w.mintB, [w.basket]));
  });

  test("a delisting refuses a basket that doesn't hold the asset", async () => {
    const w = await world();
    const other = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
    await listAsset(w.ctx, other);
    await expectError(rpc(w.ctx, delistIx(w, other, [w.basket])), "ConstituentMismatch");
  });
});

describe("basket creation", () => {
  test("rejects more than 14 constituents", async () => {
    const w = await world();
    const mints = Array.from({ length: 15 }, () => Keypair.generate().publicKey);
    const id = new Uint8Array(16).fill(4);
    const basket = pda.basket(w.creator.publicKey, id);
    const weights = [...Array(10).fill(667), ...Array(5).fill(666)];
    await expectError(
      rpc(
        w.ctx,
        w.ctx.vault.methods
          .initializeBasket(basketArgs(id, mints, weights))
          .accountsPartial({ creator: w.creator.publicKey, basket, shareMint: pda.share(basket), shareTokenProgram: TOKEN_PROGRAM_ID }),
        [w.creator],
      ),
      "TooManyConstituents",
    );
  });

  for (const [name, weights, mintsFn, fee, code] of [
    ["weights that don't sum to 100%", [6_000, 3_000], (w: World) => [w.mintA, w.mintB], 20, "WeightsNotNormalised"],
    ["the same asset twice", [5_000, 5_000], (w: World) => [w.mintA, w.mintA], 20, "DuplicateConstituent"],
    ["a creator fee above 0.20%", [5_000, 5_000], (w: World) => [w.mintA, w.mintB], 21, "FeeTooHigh"],
  ] as const) {
    test(`rejects ${name}`, async () => {
      const w = await world();
      const id = new Uint8Array(16).fill(5);
      const basket = pda.basket(w.creator.publicKey, id);
      const mints = mintsFn(w);
      await expectError(
        rpc(
          w.ctx,
          w.ctx.vault.methods
            .initializeBasket(basketArgs(id, mints, [...weights], fee))
            .accountsPartial({ creator: w.creator.publicKey, basket, shareMint: pda.share(basket), shareTokenProgram: TOKEN_PROGRAM_ID })
            .remainingAccounts(entries(mints)),
          [w.creator],
        ),
        code,
      );
    });
  }

  for (const [name, program] of [["classic", TOKEN_PROGRAM_ID], ["Token-2022", TOKEN_2022_PROGRAM_ID]] as const) {
    test(`a basket is created even if someone sent lamports to its ${name} share mint first`, async () => {
      const w = await world();
      const id = new Uint8Array(16).fill(6);
      const basket = pda.basket(w.creator.publicKey, id);
      const shareMint = pda.share(basket);
      // The address is public before the basket exists (it's in the basket's lookup table).
      w.ctx.svm.airdrop(shareMint, 5_000n);
      await rpc(
        w.ctx,
        w.ctx.vault.methods
          .initializeBasket(basketArgs(id, [w.mintA, w.mintB], [5_000, 5_000]))
          .accountsPartial({ creator: w.creator.publicKey, basket, shareMint, shareTokenProgram: program })
          .remainingAccounts(entries([w.mintA, w.mintB])),
        [w.creator],
      );
      const acc = w.ctx.svm.getAccount(shareMint)!;
      assert.ok(acc.owner.equals(program), "the share mint belongs to the token program");
      assert.equal(supply(w.ctx, shareMint), 0n);
    });
  }
});

describe("mint and redeem", () => {
  test("mint rounds deposits up and redeem rounds payouts down", async () => {
    const w = await world();
    const user = fund(w.ctx);
    const acct = await fundHolder(w, user);
    const n = 1_234_567_891n; // an awkward share amount so both roundings bite
    const a0 = balance(w.ctx, acct.a);
    const b0 = balance(w.ctx, acct.b);

    await rpc(w.ctx, mintIx(w, user, n), [user]);
    assert.equal(a0 - balance(w.ctx, acct.a), ceilDiv(UNITS_A * n, ONE_SHARE));
    assert.equal(b0 - balance(w.ctx, acct.b), ceilDiv(UNITS_B * n, ONE_SHARE));
    assert.equal(balance(w.ctx, acct.shares), n);

    const a1 = balance(w.ctx, acct.a);
    const b1 = balance(w.ctx, acct.b);
    await rpc(w.ctx, redeemIx(w, user, n), [user]);
    assert.equal(balance(w.ctx, acct.a) - a1, (UNITS_A * n) / ONE_SHARE);
    assert.equal(balance(w.ctx, acct.b) - b1, (UNITS_B * n) / ONE_SHARE);
  });

  test("minting and redeeming dust never pays out more than went in", async () => {
    const w = await world();
    const user = fund(w.ctx);
    const acct = await fundHolder(w, user);
    const a0 = balance(w.ctx, acct.a);
    const b0 = balance(w.ctx, acct.b);
    for (let i = 0; i < 5; i++) {
      await rpc(w.ctx, mintIx(w, user, 1n), [user]);
      await rpc(w.ctx, redeemIx(w, user, 1n), [user]);
    }
    assert.ok(balance(w.ctx, acct.a) <= a0);
    assert.ok(balance(w.ctx, acct.b) <= b0);
  });

  test("a constituent routed through the wrong token program is rejected", async () => {
    const w = await world();
    const bad = quads(w, w.creator.publicKey);
    bad[7] = { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false }; // mint B is Token-2022
    const ix = w.ctx.vault.methods
      .mintInKind(new BN(ONE_SHARE.toString()), new BN(ONE_SHARE.toString()))
      .accountsPartial({
        payer: w.creator.publicKey,
        basket: w.basket,
        shareMint: w.shareMint,
        payerShares: ata(w.shareMint, w.creator.publicKey),
        ...feeShares(w.shareMint),
        allowEntry: null,
        shareTokenProgram: TOKEN_PROGRAM_ID,
      })
      .remainingAccounts(bad);
    await expectError(rpc(w.ctx, ix, [w.creator]), "ConstituentMismatch");
  });

  test("a basket of two classic-SPL tokens mints and redeems", async () => {
    // Every other test mixes token programs; here the same program appears twice.
    const w = await world();
    const mintC = await createMint(w.ctx, 6, TOKEN_PROGRAM_ID);
    await listAsset(w.ctx, mintC);
    const id = new Uint8Array(16).fill(9);
    const basket = pda.basket(w.creator.publicKey, id);
    const shareMint = pda.share(basket);
    await rpc(
      w.ctx,
      w.ctx.vault.methods
        .initializeBasket(basketArgs(id, [w.mintA, mintC], [5_000, 5_000]))
        .accountsPartial({ creator: w.creator.publicKey, basket, shareMint, shareTokenProgram: TOKEN_PROGRAM_ID })
        .remainingAccounts(entries([w.mintA, mintC])),
      [w.creator],
    );
    const vA = await createAta(w.ctx, w.mintA, basket);
    const vC = await createAta(w.ctx, mintC, basket);
    const uC = await createAta(w.ctx, mintC, w.creator.publicKey);
    await mintTo(w.ctx, mintC, uC, 1_000_000_000n);
    const shares = await createAta(w.ctx, shareMint, w.creator.publicKey);
    const fees = await createFeeAccounts(w.ctx, shareMint);
    const rem = [
      [vA, ata(w.mintA, w.creator.publicKey), w.mintA],
      [vC, uC, mintC],
    ].flatMap(([v, u, m]) => [
      { pubkey: v, isSigner: false, isWritable: true },
      { pubkey: u, isSigner: false, isWritable: true },
      { pubkey: m, isSigner: false, isWritable: false },
      { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
    ]);
    const accts = { payer: w.creator.publicKey, basket, shareMint, payerShares: shares, ...fees, ...NO_GRAVI, shareTokenProgram: TOKEN_PROGRAM_ID };
    await rpc(w.ctx, w.ctx.vault.methods.seedBasket(new BN(ONE_SHARE.toString()), [new BN(1_000_000), new BN(3_000_000)]).accountsPartial(accts).remainingAccounts(rem), [w.creator]);
    assert.equal(balance(w.ctx, vC), 3_000_000n);
    const { graviMint, creatorGravi, graviTokenProgram, ...redeemAccts } = accts;
    await rpc(w.ctx, w.ctx.vault.methods.redeemInKind(new BN(ONE_SHARE.toString()), 0).accountsPartial(redeemAccts).remainingAccounts(rem), [w.creator]);
    assert.equal(balance(w.ctx, vA), 0n);
    assert.equal(balance(w.ctx, vC), 0n);
  });

  test("a private basket only lets its creator mint, but any holder can redeem", async () => {
    const w = await world({ access: "private" });
    const user = fund(w.ctx);
    const acct = await fundHolder(w, user);
    await expectError(rpc(w.ctx, mintIx(w, user, ONE_SHARE), [user]), "MintNotPermitted");

    await send(
      w.ctx,
      [createTransferCheckedInstruction(ata(w.shareMint, w.creator.publicKey), w.shareMint, acct.shares, w.creator.publicKey, ONE_SHARE, 9)],
      [w.creator],
    );
    await rpc(w.ctx, redeemIx(w, user, ONE_SHARE), [user]);
    assert.equal(balance(w.ctx, acct.shares), 0n);
  });

  test("an allowlisted basket accepts only wallets the creator added", async () => {
    const w = await world({ access: "allowlist" });
    const user = fund(w.ctx);
    await fundHolder(w, user);
    await expectError(rpc(w.ctx, mintIx(w, user, ONE_SHARE), [user]), "MintNotPermitted");

    const entry = pda.allow(w.basket, user.publicKey);
    await rpc(
      w.ctx,
      w.ctx.vault.methods.allowWallet().accountsPartial({ creator: w.creator.publicKey, basket: w.basket, wallet: user.publicKey, allowEntry: entry }),
      [w.creator],
    );
    await rpc(w.ctx, mintIx(w, user, ONE_SHARE, entry), [user]);
  });

  test("a vault left short (an issuer clawback) pays every redeemer pro rata, with no sync first", async () => {
    const w = await world();
    // An issuer clawback takes half of token B out of the vault.
    const before = balance(w.ctx, w.vaultB);
    setTokenAmount(w, w.vaultB, before / 2n);

    // Half the holder's shares get half of what's there now, not the stale promise: an early
    // redeemer can't be paid out of later ones.
    const alice = w.creator;
    const b0 = balance(w.ctx, ata(w.mintB, alice.publicKey, TOKEN_2022_PROGRAM_ID));
    await rpc(w.ctx, redeemIx(w, alice, SEED_SHARES / 2n), [alice]);
    const paid = balance(w.ctx, ata(w.mintB, alice.publicKey, TOKEN_2022_PROGRAM_ID)) - b0;
    assert.equal(paid, before / 4n);
    const [, ub] = await units(w);
    assert.equal(ub, UNITS_B / 2n, "the units now match the vault");
    await rpc(w.ctx, redeemIx(w, alice, SEED_SHARES / 2n), [alice]);
    assert.equal(balance(w.ctx, w.vaultB), 0n);
  });

  test("sync_units passes a dividend on to holders", async () => {
    const w = await world();
    const dividend = balance(w.ctx, w.vaultA) / 10n;
    await mintTo(w.ctx, w.mintA, w.vaultA, dividend, TOKEN_PROGRAM_ID);
    await rpc(
      w.ctx,
      w.ctx.vault.methods
        .syncUnits()
        .accountsPartial({ basket: w.basket, shareMint: w.shareMint })
        .remainingAccounts([w.vaultA, w.vaultB].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false }))),
    );
    const [ua] = await units(w);
    assert.equal(ua, (UNITS_A * 11n) / 10n);
  });
});

describe("fees", () => {
  test("a year of holding pays the creator's fee and nothing more to the protocol, and keeps the vault solvent", async () => {
    const w = await world({ creatorFeeBps: 20, protocolFeeBps: 40, burnShareBps: 5_000 });
    const accts = feeAccounts(w);
    // The seed's own protocol fee, already paid; holding adds nothing to it.
    const seedFee = (SEED_SHARES * 40n) / 10_000n;
    warp(w.ctx, SECONDS_PER_YEAR);
    await rpc(w.ctx, accrueIx(w, accts));

    const creatorFee = (SEED_SHARES * 20n) / 10_000n;
    assert.equal(balance(w.ctx, accts.creatorShares), SEED_SHARES - seedFee + creatorFee);
    assert.equal(balance(w.ctx, accts.burnShares), seedFee / 2n);
    assert.equal(balance(w.ctx, accts.treasuryShares), seedFee - seedFee / 2n);

    const total = supply(w.ctx, w.shareMint);
    assert.equal(total, SEED_SHARES + creatorFee);
    const [ua, ub] = await units(w);
    assert.ok((ua * total) / ONE_SHARE <= balance(w.ctx, w.vaultA), "vault short of token A");
    assert.ok((ub * total) / ONE_SHARE <= balance(w.ctx, w.vaultB), "vault short of token B");
  });




  test("the creator's opening deposit pays the protocol fee like any mint", async () => {
    // Fee-free, a basket of one's own would be the way to buy in without paying it.
    const w = await world({ protocolFeeBps: 40 });
    const accts = feeAccounts(w);
    const fee = (SEED_SHARES * 40n) / 10_000n;
    assert.equal(balance(w.ctx, accts.creatorShares), SEED_SHARES - fee);
    assert.equal(balance(w.ctx, accts.treasuryShares) + balance(w.ctx, accts.burnShares), fee);
  });


  test("minting keeps 0.4% of the new shares for the protocol, split between burn and treasury", async () => {
    const w = await world({ protocolFeeBps: 40, burnShareBps: 7_500 });
    const accts = feeAccounts(w);
    const alice = fund(w.ctx);
    const acct = await fundHolder(w, alice);
    const a0 = balance(w.ctx, acct.a);
    const [burn0, treasury0] = [balance(w.ctx, accts.burnShares), balance(w.ctx, accts.treasuryShares)];
    const shares = 10n * ONE_SHARE;
    await rpc(w.ctx, mintIx(w, alice, shares), [alice]);

    const fee = (shares * 40n) / 10_000n;
    assert.equal(balance(w.ctx, acct.shares), shares - fee);
    assert.equal(balance(w.ctx, accts.burnShares) - burn0, (fee * 3n) / 4n);
    assert.equal(balance(w.ctx, accts.treasuryShares) - treasury0, fee - (fee * 3n) / 4n);
    // Alice paid the underlying for every share, fee included, so the vault stays fully backed.
    assert.equal(a0 - balance(w.ctx, acct.a), ceilDiv(UNITS_A * shares, ONE_SHARE));
    const [ua, ub] = await units(w);
    const total = supply(w.ctx, w.shareMint);
    assert.ok((ua * total) / ONE_SHARE <= balance(w.ctx, w.vaultA), "vault short of token A");
    assert.ok((ub * total) / ONE_SHARE <= balance(w.ctx, w.vaultB), "vault short of token B");
  });

  test("redeeming keeps 0.4% of the shares handed in and pays out the rest", async () => {
    const w = await world({ protocolFeeBps: 40, burnShareBps: 7_500 });
    const accts = feeAccounts(w);
    const holder = ata(w.shareMint, w.creator.publicKey);
    const a0 = balance(w.ctx, ata(w.mintA, w.creator.publicKey));
    const supply0 = supply(w.ctx, w.shareMint);
    const held0 = balance(w.ctx, holder);
    const fees0 = balance(w.ctx, accts.burnShares) + balance(w.ctx, accts.treasuryShares);
    const shares = 5n * ONE_SHARE;
    await rpc(w.ctx, redeemIx(w, w.creator, shares), [w.creator]);

    const fee = (shares * 40n) / 10_000n;
    assert.equal(balance(w.ctx, holder), held0 - shares);
    assert.equal(balance(w.ctx, accts.burnShares) + balance(w.ctx, accts.treasuryShares) - fees0, fee);
    assert.equal(supply(w.ctx, w.shareMint), supply0 - (shares - fee));
    assert.equal(balance(w.ctx, ata(w.mintA, w.creator.publicKey)) - a0, (UNITS_A * (shares - fee)) / ONE_SHARE);
  });

  test("the protocol fee can't be set above 0.5% per mint or redeem", async () => {
    const ctx = setup();
    await expectError(initProtocol(ctx, 51, 5_000), "FeeTooHigh");
  });

  test("the burn's share of the protocol fee can't be set under half", async () => {
    await expectError(initProtocol(setup(), 40, 4_999), "BurnShareTooLow");
    const w = await world({ protocolFeeBps: 40 });
    const setFees = (burn: number) =>
      w.ctx.vault.methods.setProtocolFees(40, burn).accountsPartial({ authority: w.ctx.payer.publicKey, protocol: pda.protocol() });
    await expectError(rpc(w.ctx, setFees(0)), "BurnShareTooLow");
    await expectError(rpc(w.ctx, setFees(4_999)), "BurnShareTooLow");
    await rpc(w.ctx, setFees(10_000));
    await rpc(w.ctx, setFees(5_000));
  });

  const policyIx = (w: World, feeBps: number) =>
    w.ctx.vault.methods
      .setPolicy({ hybrid: {} }, new BN(30 * 86_400), 500, feeBps)
      .accountsPartial({ authority: w.creator.publicKey, basket: w.basket })
      .instruction();

  test("a creator fee set after a year at 0% starts then, not from launch", async () => {
    const w = await world({ creatorFeeBps: 0 });
    const accts = feeAccounts(w);
    warp(w.ctx, SECONDS_PER_YEAR);
    await send(w.ctx, [await policyIx(w, 20)], [w.creator]);
    await rpc(w.ctx, accrueIx(w, accts));
    assert.equal(balance(w.ctx, accts.creatorShares), SEED_SHARES, "nothing owed for the year at 0%");

    warp(w.ctx, SECONDS_PER_YEAR);
    await rpc(w.ctx, accrueIx(w, accts));
    assert.equal(balance(w.ctx, accts.creatorShares), SEED_SHARES + (SEED_SHARES * 20n) / 10_000n);
  });

  test("at 0% accruing moves the fee clock on", async () => {
    const w = await world({ creatorFeeBps: 0 });
    warp(w.ctx, 1_000);
    await rpc(w.ctx, accrueIx(w, feeAccounts(w)));
    const b: any = await w.ctx.vault.account.basket.fetch(w.basket);
    assert.equal(b.lastFeeAccrual.toNumber(), now(w.ctx));
  });

  test("a fee change restarts the fee clock; accruing first in the same transaction collects the old rate", async () => {
    const w = await world({ creatorFeeBps: 20 });
    const accts = feeAccounts(w);
    warp(w.ctx, SECONDS_PER_YEAR);
    await send(w.ctx, [await accrueIx(w, accts).instruction(), await policyIx(w, 10)], [w.creator]);
    const yearAt20 = (SEED_SHARES * 20n) / 10_000n;
    assert.equal(balance(w.ctx, accts.creatorShares), SEED_SHARES + yearAt20);

    // The new rate only counts from the change.
    warp(w.ctx, SECONDS_PER_YEAR);
    await rpc(w.ctx, accrueIx(w, accts));
    const yearAt10 = ((SEED_SHARES + yearAt20) * 10n) / 10_000n;
    assert.equal(balance(w.ctx, accts.creatorShares), SEED_SHARES + yearAt20 + yearAt10);
  });

  test("a policy change that keeps the fee doesn't touch the fee clock", async () => {
    const w = await world({ creatorFeeBps: 20 });
    const before: any = await w.ctx.vault.account.basket.fetch(w.basket);
    warp(w.ctx, 1_000);
    await send(w.ctx, [await policyIx(w, 20)], [w.creator]);
    const after: any = await w.ctx.vault.account.basket.fetch(w.basket);
    assert.equal(after.lastFeeAccrual.toString(), before.lastFeeAccrual.toString());
  });

  test("burn-earmarked shares cannot be pulled out through the treasury", async () => {
    const w = await world({ protocolFeeBps: 40 });
    const accts = feeAccounts(w);
    const alice = fund(w.ctx);
    await fundHolder(w, alice);
    await rpc(w.ctx, mintIx(w, alice, 10n * ONE_SHARE), [alice]);
    const dest = await createAta(w.ctx, w.shareMint, w.ctx.payer.publicKey);
    await expectError(
      rpc(
        w.ctx,
        w.ctx.vault.methods.treasuryWithdraw(new BN(1)).accountsPartial({
          authority: w.ctx.payer.publicKey,
          protocol: pda.protocol(),
          shareMint: w.shareMint,
          treasuryShares: accts.burnShares,
          destination: dest,
          shareTokenProgram: TOKEN_PROGRAM_ID,
        }),
      ),
      "ConstraintTokenOwner",
    );
  });
});

describe("mandate control", () => {
  const targets = (w: World, who: Keypair, t: number[]) =>
    w.ctx.vault.methods.setTargets(t).accountsPartial({ authority: who.publicKey, basket: w.basket });

  test("an immutable basket can never be reconfigured", async () => {
    const w = await world({ control: "immutable" });
    await expectError(rpc(w.ctx, targets(w, w.creator, [5_000, 5_000]), [w.creator]), "BasketImmutable");
  });

  test("a creator-controlled basket accepts its creator and nobody else", async () => {
    const w = await world({ control: "creator" });
    const outsider = fund(w.ctx);
    await expectError(rpc(w.ctx, targets(w, outsider, [5_000, 5_000]), [outsider]), "NotMandateAuthority");
    await rpc(w.ctx, targets(w, w.creator, [5_000, 5_000]), [w.creator]);
    const b: any = await w.ctx.vault.account.basket.fetch(w.basket);
    assert.deepEqual(b.constituents.map((c: any) => c.targetBps), [5_000, 5_000]);
  });

  test("a policy can't trade more than daily or on less than 0.5% drift", async () => {
    const w = await world();
    const policy = (mode: string, secs: number, drift: number) =>
      rpc(w.ctx, w.ctx.vault.methods.setPolicy({ [mode]: {} } as never, new BN(secs), drift, 10).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }), [w.creator]);
    await expectError(policy("hybrid", 3_600, 500), "BadPolicy");
    await expectError(policy("hybrid", 86_400, 10), "BadPolicy");
    await expectError(policy("drift", 0, 0), "BadPolicy");
    await expectError(policy("periodic", 0, 0), "BadPolicy");
    // Schedule-only ignores drift, drift-only ignores the schedule.
    await policy("periodic", 7 * 86_400, 0);
    await policy("drift", 0, 50);
    await policy("hybrid", 86_400, 50);
  });

  test("the creator fee cannot be raised above 0.20% later either", async () => {
    const w = await world();
    await expectError(
      rpc(
        w.ctx,
        w.ctx.vault.methods.setPolicy({ hybrid: {} }, new BN(86_400), 500, 21).accountsPartial({ authority: w.creator.publicKey, basket: w.basket }),
        [w.creator],
      ),
      "FeeTooHigh",
    );
  });
});

function basketArgs(id: Uint8Array, mints: PublicKey[], targetBps: number[], fee = 20) {
  return {
    basketId: Array.from(id),
    access: { public: {} },
    control: { creator: {} },
    rebalanceMode: { hybrid: {} },
    intervalSecs: new BN(86_400),
    driftBps: 500,
    mgmtFeeBps: fee,
    mints,
    targetBps,
  };
}

function entries(mints: PublicKey[]) {
  return mints.flatMap((m) => [
    { pubkey: m, isSigner: false, isWritable: false },
    { pubkey: pda.asset(m), isSigner: false, isWritable: false },
  ]);
}
