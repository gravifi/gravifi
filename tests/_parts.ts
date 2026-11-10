// Shared by tests/parts.test.ts and tests/nov-upgrade.test.ts: a part's OFT accounts written by hand
// (laid out as contracts/solana-oft's OFTStore and PeerConfig are), and a basket of three Verified
// Solana tokens and a part.
import { createSetAuthorityInstruction, AuthorityType } from "@solana/spl-token";
import type { Keypair } from "@solana/web3.js";
import { BN, PublicKey, SystemProgram, TOKEN_PROGRAM_ID, createAta, createMint, fund, pda, rpc, send, setup, type Ctx } from "./helpers.ts";
import { NO_GRAVI, initProtocol, listAsset, setRole, setRoleIx, setParamIx, ROLE, CONFIG, skipNotices } from "./fixtures.ts";

export const DAY = 86_400;
export const EID = 30_000 + 168; // any remote eid: the peer's address is derived from it
export const OFT = new PublicKey("EFeeeMLaSLpEffo3qp55MaqvmjaVmSrRLZoaWevBgcHx");
const STORE_DISC = [195, 215, 104, 134, 185, 195, 240, 114];
const PEER_DISC = [181, 157, 86, 198, 33, 193, 94, 203];
export const MANDATE = Array.from({ length: 32 }, (_, i) => i + 1);

export type Guard = { maxDayBps: number; maxWeekBps: number; maxWeightBps: number; maxUnverifiedBps: number; minAssets: number; maxAssets: number; stopBps: number; stopTrailing: boolean };
export const GUARD: Guard = { maxDayBps: 2_000, maxWeekBps: 5_000, maxWeightBps: 3_500, maxUnverifiedBps: 3_000, minAssets: 2, maxAssets: 6, stopBps: 0, stopTrailing: false };

export const peerOf = (store: PublicKey, eid = EID, program = OFT) => {
  const e = Buffer.alloc(4);
  e.writeUInt32BE(eid);
  return PublicKey.findProgramAddressSync([Buffer.from("Peer"), store.toBuffer(), e], program)[0];
};

function put(ctx: Ctx, key: PublicKey, owner: PublicKey, data: Buffer) {
  ctx.svm.setAccount(key, { lamports: Number(ctx.svm.minimumBalanceForRentExemption(BigInt(data.length))), data, owner, executable: false });
}

/** An OFTStore for `mint` run by `admin`, and its peer on `EID` (pointing at an EVM token), owned by
 *  `program`; the mint's authority handed to the store. What can be bent per test. */
export async function wirePart(ctx: Ctx, mint: PublicKey, admin: PublicKey, o: { program?: PublicKey; storeMint?: PublicKey; peerAddress?: Buffer; peerEid?: number; noPeer?: boolean; keepAuthority?: boolean } = {}) {
  const program = o.program ?? OFT;
  const escrow = PublicKey.unique();
  const [store] = PublicKey.findProgramAddressSync([Buffer.from("OFT"), escrow.toBuffer()], program);
  const s = Buffer.alloc(8 + 215);
  Buffer.from(STORE_DISC).copy(s, 0);
  s.writeBigUInt64LE(1n, 9); // ld2sd_rate
  (o.storeMint ?? mint).toBuffer().copy(s, 17);
  escrow.toBuffer().copy(s, 49);
  admin.toBuffer().copy(s, 122);
  put(ctx, store, program, s);
  const peer = peerOf(store, o.peerEid ?? EID, program);
  if (!o.noPeer) {
    const p = Buffer.alloc(8 + 32 + 64);
    Buffer.from(PEER_DISC).copy(p, 0);
    (o.peerAddress ?? Buffer.concat([Buffer.alloc(12), Buffer.alloc(20, 0xab)])).copy(p, 8);
    put(ctx, peer, program, p);
  }
  // The mint's own token program (classic or Token-2022).
  const tokenProgram = new PublicKey(ctx.svm.getAccount(mint)!.owner);
  if (!o.keepAuthority) await send(ctx, [createSetAuthorityInstruction(mint, ctx.payer.publicKey, AuthorityType.MintTokens, store, [], tokenProgram)]);
  return { store, peer: peerOf(store, EID, program) };
}

export const verifyPart = (ctx: Ctx, mint: PublicKey, store: PublicKey, peer: PublicKey, payer: Keypair, eid = EID) =>
  rpc(
    ctx,
    ctx.vault.methods
      .verifyPart(eid)
      .accountsPartial({ payer: payer.publicKey, parts: pda.parts(), mint, oftStore: store, peer, verified: pda.verified(mint), systemProgram: SystemProgram.programId }),
    [payer],
  );

/** Three Verified Solana tokens and a part P (listed, no record yet), in a creator basket not seeded. */
export async function world(targetBps: number[] = [2_500, 2_500, 2_000, 3_000], program?: string) {
  const ctx = setup(program);
  await initProtocol(ctx, 40);
  const mints: PublicKey[] = [];
  for (let i = 0; i < 4; i++) {
    const m = await createMint(ctx, i === 3 ? 9 : 6, TOKEN_PROGRAM_ID);
    await listAsset(ctx, m);
    mints.push(m);
  }
  const [A, B, C, P] = mints;
  const creator = fund(ctx);
  const strategist = fund(ctx);
  const admin = fund(ctx);
  await rpc(ctx, setRoleIx(ctx, ROLE.strategist, strategist.publicKey)); skipNotices(ctx);
  await rpc(ctx, setParamIx(ctx, CONFIG.smartFee, 80)); skipNotices(ctx);
  for (const m of [A, B, C]) await rpc(ctx, ctx.vault.methods.setVerified(true).accountsPartial({ authority: ctx.payer.publicKey, protocol: pda.protocol(), mint: m, verified: pda.verified(m) }));
  const basketId = new Uint8Array(16);
  basketId[0] = 9;
  const basket = pda.basket(creator.publicKey, basketId);
  await rpc(
    ctx,
    ctx.vault.methods
      .initializeBasket({ basketId: Array.from(basketId), access: { public: {} }, control: { creator: {} }, rebalanceMode: { drift: {} }, intervalSecs: new BN(0), driftBps: 500, mgmtFeeBps: 0, mints, targetBps })
      .accountsPartial({ ...NO_GRAVI, creator: creator.publicKey, basket, shareMint: pda.share(basket), shareTokenProgram: TOKEN_PROGRAM_ID })
      .remainingAccounts(mints.flatMap((m) => [m, pda.asset(m)].map((pubkey) => ({ pubkey, isSigner: false, isWritable: false })))),
    [creator],
  );
  for (const m of mints) await createAta(ctx, m, basket, TOKEN_PROGRAM_ID);
  return { ctx, A, B, C, P, mints, creator, strategist, admin, basket };
}
export type W = Awaited<ReturnType<typeof world>>;

export const records = (w: W) => w.mints.map((m) => ({ pubkey: pda.verified(m), isSigner: false, isWritable: false }));
export const enable = (w: W, guard: Guard) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods
      .enableSmart(MANDATE, guard)
      .accountsPartial({ creator: w.creator.publicKey, basket: w.basket, smart: pda.smart(w.basket), protocol: pda.protocol(), creationBurn: pda.creationBurn(), ...NO_GRAVI })
      .remainingAccounts(records(w)),
    [w.creator],
  );
export const setTargets = (w: W, bps: number[]) =>
  rpc(
    w.ctx,
    w.ctx.vault.methods.smartSetTargets(bps).accountsPartial({ strategist: w.strategist.publicKey, strategistConfig: pda.strategist(), basket: w.basket, smart: pda.smart(w.basket) }).remainingAccounts(records(w)),
    [w.strategist],
  );
export const targets = async (w: W) => ((await w.ctx.vault.account.basket.fetch(w.basket)).constituents as { targetBps: number }[]).map((c) => c.targetBps);
export const hasRecord = (w: W, m: PublicKey) => {
  const a = w.ctx.svm.getAccount(pda.verified(m));
  return !!a && a.owner.equals(w.ctx.vault.programId) && a.data.length > 0;
};

export async function named(w: W) {
  await setRole(w.ctx, ROLE.partAdmin, w.admin.publicKey);
  await setRole(w.ctx, ROLE.partOft, OFT);
}

