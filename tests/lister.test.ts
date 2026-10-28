// Open listing's on-chain half: the lister, the one key besides the protocol authority that may
// list assets (the app's listing key, co-signing inside a creator's own transaction), and what it
// may not list. The creator pays the record's rent.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import {
  ExtensionType,
  TOKEN_2022_PROGRAM_ID,
  createInitializeMint2Instruction,
  createInitializeNonTransferableMintInstruction,
  createPauseInstruction,
  getMintLen,
} from "@solana/spl-token";
import { createMint, expectError, fund, pda, rpc, send, setup, type Ctx } from "./helpers.ts";
import { initProtocol, listAsset } from "./fixtures.ts";

async function setLister(ctx: Ctx, lister: PublicKey, authority = ctx.payer) {
  await rpc(ctx, ctx.vault.methods.setLister(lister).accountsPartial({ authority: authority.publicKey, protocol: pda.protocol(), lister: pda.lister() }), [authority]);
}

/** A listing as the app makes it: the creator pays, the lister signs as authority. */
function listByLister(ctx: Ctx, mint: PublicKey, lister: Keypair, creator: Keypair, withListerAccount = true) {
  return rpc(
    ctx,
    ctx.vault.methods
      .registerAsset()
      .accountsPartial({ payer: creator.publicKey, authority: lister.publicKey, protocol: pda.protocol(), mint, assetEntry: pda.asset(mint) })
      .remainingAccounts(withListerAccount ? [{ pubkey: pda.lister(), isSigner: false, isWritable: false }] : []),
    [creator, lister],
  );
}

async function world() {
  const ctx = setup();
  await initProtocol(ctx);
  const lister = fund(ctx, 1_000_000n);
  const creator = fund(ctx);
  await setLister(ctx, lister.publicKey);
  return { ctx, lister, creator };
}

const listedCount = async (ctx: Ctx) => (await ctx.vault.account.protocol.fetch(pda.protocol())).listedCount as number;

describe("the lister", () => {
  it("lists a token in the creator's transaction, the creator paying the rent", async () => {
    const { ctx, lister, creator } = await world();
    const mint = await createMint(ctx, 6);
    const before = { creator: ctx.svm.getBalance(creator.publicKey)!, lister: ctx.svm.getBalance(lister.publicKey)!, count: await listedCount(ctx) };
    await listByLister(ctx, mint, lister, creator);
    const entry = await ctx.vault.account.assetEntry.fetch(pda.asset(mint));
    assert.ok((entry.mint as PublicKey).equals(mint));
    assert.deepEqual(entry.status, { listed: {} });
    assert.equal(await listedCount(ctx), before.count + 1);
    assert.equal(ctx.svm.getBalance(lister.publicKey), before.lister, "the lister pays nothing");
    assert.ok(ctx.svm.getBalance(creator.publicKey)! < before.creator, "the creator paid the rent");
  });

  it("can't list without its account, and nobody else can with it", async () => {
    const { ctx, lister, creator } = await world();
    const mint = await createMint(ctx, 6);
    await expectError(listByLister(ctx, mint, lister, creator, false), "NotProtocolAuthority");
    const stranger = fund(ctx);
    await expectError(listByLister(ctx, mint, stranger, creator), "NotProtocolAuthority");
    // Another account passed as the lister's isn't taken for it.
    await expectError(
      rpc(
        ctx,
        ctx.vault.methods
          .registerAsset()
          .accountsPartial({ payer: creator.publicKey, authority: lister.publicKey, protocol: pda.protocol(), mint, assetEntry: pda.asset(mint) })
          .remainingAccounts([{ pubkey: pda.protocol(), isSigner: false, isWritable: false }]),
        [creator, lister],
      ),
      "NotProtocolAuthority",
    );
  });

  it("is named by the protocol authority only, and can be removed", async () => {
    const { ctx, lister, creator } = await world();
    const stranger = fund(ctx);
    await expectError(setLister(ctx, stranger.publicKey, stranger), "NotProtocolAuthority");
    await setLister(ctx, PublicKey.default);
    await expectError(listByLister(ctx, await createMint(ctx, 6), lister, creator), "NotProtocolAuthority");
    await setLister(ctx, lister.publicKey);
    await listByLister(ctx, await createMint(ctx, 6), lister, creator);
  });

  it("refuses what a vault can't hold: hook, steep fee, frozen-by-default, paused, non-transferable", async () => {
    const { ctx, lister, creator } = await world();
    const hooked = await createMint(ctx, 6, TOKEN_2022_PROGRAM_ID, Keypair.generate().publicKey);
    await expectError(listByLister(ctx, hooked, lister, creator), "TransferHookUnsupported");
    const steep = await createMint(ctx, 6, TOKEN_2022_PROGRAM_ID, undefined, 500);
    await expectError(listByLister(ctx, steep, lister, creator), "TransferFeeTooHigh");
    const frozen = await createMint(ctx, 6, TOKEN_2022_PROGRAM_ID, undefined, undefined, { startsFrozen: true });
    await expectError(listByLister(ctx, frozen, lister, creator), "DefaultFrozen");
    const pausable = await createMint(ctx, 6, TOKEN_2022_PROGRAM_ID, undefined, undefined, { pausable: true });
    await send(ctx, [createPauseInstruction(pausable, ctx.payer.publicKey, [], TOKEN_2022_PROGRAM_ID)]);
    await expectError(listByLister(ctx, pausable, lister, creator), "TokenPaused");
    // Not paused: a stock issuer's pause key alone is fine.
    await listByLister(ctx, await createMint(ctx, 6, TOKEN_2022_PROGRAM_ID, undefined, undefined, { pausable: true }), lister, creator);
    const soulbound = Keypair.generate();
    const space = getMintLen([ExtensionType.NonTransferable]);
    await send(
      ctx,
      [
        SystemProgram.createAccount({ fromPubkey: ctx.payer.publicKey, newAccountPubkey: soulbound.publicKey, space, lamports: Number(ctx.svm.minimumBalanceForRentExemption(BigInt(space))), programId: TOKEN_2022_PROGRAM_ID }),
        createInitializeNonTransferableMintInstruction(soulbound.publicKey, TOKEN_2022_PROGRAM_ID),
        createInitializeMint2Instruction(soulbound.publicKey, 6, ctx.payer.publicKey, null, TOKEN_2022_PROGRAM_ID),
      ],
      [soulbound],
    );
    await expectError(listByLister(ctx, soulbound.publicKey, lister, creator), "NonTransferable");
  });

  it("leaves the protocol authority's listing as it was: it may still list a token that starts frozen", async () => {
    const { ctx } = await world();
    await listAsset(ctx, await createMint(ctx, 6));
    await listAsset(ctx, await createMint(ctx, 6, TOKEN_2022_PROGRAM_ID, undefined, undefined, { startsFrozen: true }));
  });
});
