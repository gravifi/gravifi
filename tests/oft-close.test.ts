// GraviFi's `close_oft` in the OFT program (contracts/solana-oft/programs/oft/src/instructions/close_oft.rs):
// a finished part's store, LzReceiveTypes account, escrow and peers give back their rent.
//
// Rehearsed as the upgrade will run: a part's store made by the OFT program on mainnet
// (target/deploy/oft-onchain.so: `solana program dump -u m EFeeeMLaSLpEffo3qp55MaqvmjaVmSrRLZoaWevBgcHx
// target/deploy/oft-onchain.so`), registered with LayerZero's mainnet endpoint (target/deploy/lz-endpoint.so:
// `solana program dump -u m 76y77prsiCMvXMjuoZ5VRrhG5qYBrUMYTE5WgHqgjEn6 target/deploy/lz-endpoint.so`),
// then this build (contracts/solana-oft/target/deploy/oft.so, built with OFT_ID=EFeee…) loaded over it.
// Skipped without those files.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { LiteSVM, FailedTransactionMetadata } from "litesvm";
import { Keypair, PublicKey, SystemProgram, Transaction, TransactionInstruction } from "@solana/web3.js";
import { MINT_SIZE, TOKEN_PROGRAM_ID, createInitializeMint2Instruction } from "@solana/spl-token";

const root = new URL("..", import.meta.url).pathname;
const ONCHAIN = `${root}target/deploy/oft-onchain.so`;
const ENDPOINT_SO = `${root}target/deploy/lz-endpoint.so`;
const BUILD = `${root}contracts/solana-oft/target/deploy/oft.so`;
const OFT = new PublicKey("EFeeeMLaSLpEffo3qp55MaqvmjaVmSrRLZoaWevBgcHx");
const ENDPOINT = new PublicKey("76y77prsiCMvXMjuoZ5VRrhG5qYBrUMYTE5WgHqgjEn6");
const RH_EID = 30_000 + 333; // any remote endpoint id: the program only seeds the peer with it
const missing = [ONCHAIN, ENDPOINT_SO, BUILD].filter((f) => !existsSync(f));

const disc = (name: string) => createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
const pda = (seeds: (Buffer | Uint8Array)[], program = OFT) => PublicKey.findProgramAddressSync(seeds, program)[0];
const u32le = (n: number) => { const b = Buffer.alloc(4); b.writeUInt32LE(n); return b; };
const u32be = (n: number) => { const b = Buffer.alloc(4); b.writeUInt32BE(n); return b; };
const meta = (pubkey: PublicKey, isWritable = false, isSigner = false) => ({ pubkey, isSigner, isWritable });

type World = ReturnType<typeof setup>;

function setup() {
  const svm = new LiteSVM();
  svm.addProgramFromFile(OFT, ONCHAIN);
  svm.addProgramFromFile(ENDPOINT, ENDPOINT_SO);
  const payer = Keypair.generate(); // the creator: pays the store's rent, as server/partSetup.ts has them
  const admin = Keypair.generate(); // the store's admin: the app's server key
  for (const k of [payer, admin]) svm.airdrop(k.publicKey, 100_000_000_000n);
  return { svm, payer, admin };
}

function send(w: World, ixs: TransactionInstruction[], signers: Keypair[]) {
  const tx = new Transaction().add(...ixs);
  tx.recentBlockhash = w.svm.latestBlockhash();
  tx.feePayer = signers[0].publicKey;
  tx.sign(...signers);
  const r = w.svm.sendTransaction(tx);
  w.svm.expireBlockhash();
  if (r instanceof FailedTransactionMetadata) throw new Error(`${r.err()}\n${r.meta().logs().join("\n")}`);
}

async function expectFail(f: () => void, text: string) {
  try {
    f();
  } catch (e) {
    assert.ok(String(e).includes(text), `expected "${text}", got:\n${e}`);
    return;
  }
  assert.fail(`expected "${text}", but it went through`);
}

const lamports = (w: World, k: PublicKey) => BigInt(w.svm.getAccount(k)?.lamports ?? 0);
const gone = (w: World, k: PublicKey) => (w.svm.getAccount(k)?.data.length ?? 0) === 0 && lamports(w, k) === 0n;

/** A part's Solana side as partSetup.ts makes it: the mint (authority the store), the store with its
 *  escrow (registered with the endpoint), then the peer with its enforced options. */
function part(w: World) {
  const mint = Keypair.generate();
  const escrow = Keypair.generate();
  const store = pda([Buffer.from("OFT"), escrow.publicKey.toBuffer()]);
  const types = pda([Buffer.from("LzReceiveTypes"), store.toBuffer()]);
  const registry = pda([Buffer.from("OApp"), store.toBuffer()], ENDPOINT);
  const eventAuthority = pda([Buffer.from("__event_authority")], ENDPOINT);
  const peer = pda([Buffer.from("Peer"), store.toBuffer(), u32be(RH_EID)]);
  const rent = w.svm.minimumBalanceForRentExemption(BigInt(MINT_SIZE));
  const initOft = new TransactionInstruction({
    programId: OFT,
    keys: [
      meta(w.payer.publicKey, true, true), meta(store, true), meta(types, true), meta(mint.publicKey), meta(escrow.publicKey, true, true), meta(TOKEN_PROGRAM_ID), meta(SystemProgram.programId),
      // register_oapp: [endpoint, payer, oapp, registry, system, event authority, program]
      meta(ENDPOINT), meta(w.payer.publicKey, true, true), meta(store), meta(registry, true), meta(SystemProgram.programId), meta(eventAuthority), meta(ENDPOINT),
    ],
    // InitOFTParams { oft_type: Native, admin, shared_decimals: 6, endpoint_program: None }
    data: Buffer.concat([disc("init_oft"), Buffer.from([0]), w.admin.publicKey.toBuffer(), Buffer.from([6, 0])]),
  });
  send(
    w,
    [
      SystemProgram.createAccount({ fromPubkey: w.payer.publicKey, newAccountPubkey: mint.publicKey, lamports: Number(rent), space: MINT_SIZE, programId: TOKEN_PROGRAM_ID }),
      createInitializeMint2Instruction(mint.publicKey, 9, store, null),
      initOft,
    ],
    [w.payer, mint, escrow],
  );
  const setPeer = (config: Buffer) =>
    new TransactionInstruction({
      programId: OFT,
      keys: [meta(w.admin.publicKey, true, true), meta(peer, true), meta(store), meta(SystemProgram.programId)],
      data: Buffer.concat([disc("set_peer_config"), u32le(RH_EID), config]),
    });
  const options = Buffer.from([0, 3]); // type-3 options, empty
  const vec = (b: Buffer) => Buffer.concat([u32le(b.length), b]);
  send(w, [setPeer(Buffer.concat([Buffer.from([0]), Buffer.alloc(32, 7)])), setPeer(Buffer.concat([Buffer.from([2]), vec(options), vec(options)]))], [w.admin]);
  return { mint: mint.publicKey, escrow, store, types, registry, peer };
}

type Part = ReturnType<typeof part>;

const closeIx = (w: World, p: Part, signer: PublicKey, rentTo: PublicKey, peers: { key: PublicKey; eid: number }[] = [{ key: p.peer, eid: RH_EID }]) =>
  new TransactionInstruction({
    programId: OFT,
    keys: [meta(signer, false, true), meta(rentTo, true), meta(p.store, true), meta(p.types, true), meta(p.mint), meta(p.escrow.publicKey, true), meta(TOKEN_PROGRAM_ID), ...peers.map((x) => meta(x.key, true))],
    data: Buffer.concat([disc("close_oft"), u32le(peers.length), ...peers.map((x) => u32le(x.eid))]),
  });

/** Writes a u64 into an account's data (a token account's amount at 64, a mint's supply at 36). */
function poke(w: World, k: PublicKey, offset: number, value: bigint) {
  const a = w.svm.getAccount(k)!;
  const data = Buffer.from(a.data);
  data.writeBigUInt64LE(value, offset);
  w.svm.setAccount(k, { ...a, data });
}

describe("OFT close_oft: a finished part's Solana side gives back its rent", { skip: missing.length ? `missing ${missing.join(", ")}` : false }, () => {
  it("a store made by the program on mainnet closes after the upgrade, every lamport to the creator", async () => {
    const w = setup();
    const p = part(w);
    const creator = w.payer.publicKey;
    // The program on mainnet has no close.
    await expectFail(() => send(w, [closeIx(w, p, w.admin.publicKey, creator)], [w.admin]), "InstructionFallbackNotFound");

    w.svm.addProgramFromFile(OFT, BUILD);
    const ours = [p.store, p.types, p.escrow.publicKey, p.peer];
    const rent = ours.reduce((s, k) => s + lamports(w, k), 0n);
    const before = lamports(w, creator);
    send(w, [closeIx(w, p, w.admin.publicKey, creator)], [w.admin]);
    for (const k of ours) assert.ok(gone(w, k), `${k.toBase58()} closed`);
    assert.equal(lamports(w, creator) - before, rent, "every lamport of rent to the creator");
    // About 0.0134 SOL a part at today's rent: store 223 bytes, LzReceiveTypes 72, peer 1,654, escrow 165.
    assert.ok(rent > 10_000_000n, `rent ${rent}`);
    // The endpoint's registry is LayerZero's: it stays, and with it the store's address can't be
    // registered again, so the mint (its authority the closed store) never mints again.
    assert.ok(lamports(w, p.registry) > 0n);
    assert.ok(lamports(w, p.mint) > 0n);
  });

  it("is the admin's alone", async () => {
    const w = setup();
    const p = part(w);
    w.svm.addProgramFromFile(OFT, BUILD);
    const eve = Keypair.generate();
    w.svm.airdrop(eve.publicKey, 1_000_000_000n);
    await expectFail(() => send(w, [closeIx(w, p, eve.publicKey, eve.publicKey)], [eve]), "Unauthorized");
    // The creator who paid isn't the admin either: the app's server co-signs once the part is finished.
    await expectFail(() => send(w, [closeIx(w, p, w.payer.publicKey, w.payer.publicKey)], [w.payer]), "Unauthorized");
  });

  it("waits while any part share is on Solana, or anything is in the escrow", async () => {
    const w = setup();
    const p = part(w);
    w.svm.addProgramFromFile(OFT, BUILD);
    const close = () => send(w, [closeIx(w, p, w.admin.publicKey, w.payer.publicKey)], [w.admin]);
    poke(w, p.mint, 36, 1n);
    await expectFail(close, "PartNotFinished");
    poke(w, p.mint, 36, 0n);
    poke(w, p.escrow.publicKey, 64, 1n);
    await expectFail(close, "PartNotFinished");
    poke(w, p.escrow.publicKey, 64, 0n);
    close();
    assert.ok(gone(w, p.store));
  });

  it("closes only the store's own peers, named with their endpoint ids", async () => {
    const w = setup();
    const p = part(w);
    const other = part(w);
    w.svm.addProgramFromFile(OFT, BUILD);
    const close = (peers: { key: PublicKey; eid: number }[]) => send(w, [closeIx(w, p, w.admin.publicKey, w.payer.publicKey, peers)], [w.admin]);
    await expectFail(() => close([{ key: p.peer, eid: RH_EID + 1 }]), "InvalidPeer");
    await expectFail(() => close([{ key: other.peer, eid: RH_EID }]), "InvalidPeer");
    // Without its peers named, the store still closes; a peer left behind keeps its rent.
    close([]);
    assert.ok(gone(w, p.store) && gone(w, p.types) && gone(w, p.escrow.publicKey));
    assert.ok(lamports(w, p.peer) > 0n);
    assert.ok(lamports(w, other.store) > 0n, "the other part is untouched");
  });
});
