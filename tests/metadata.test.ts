// The share token's metadata (name, ticker, logo URI), written into Metaplex Token Metadata so
// wallets show the shares. Runs the real Metaplex program, dumped from mainnet
// (scripts/dump-metaplex.sh).
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { Keypair, PublicKey, SystemProgram, TransactionInstruction } from "@solana/web3.js";
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, expectError, fund, rpc, send } from "./helpers.ts";
import { world, type World, type WorldOpts } from "./fixtures.ts";

const MPL = new PublicKey("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
const SYSVAR_INSTRUCTIONS = new PublicKey("Sysvar1nstructions1111111111111111111111111");
const MPL_SO = new URL("../target/deploy/mpl_token_metadata.so", import.meta.url).pathname;
const METAPLEX_FEE = 10_000_000; // 0.01 SOL, charged on create
const URI = "https://app.gravifi.xyz/api/basket-meta/x?v=1";

/** Skipped, saying how to run them, until Metaplex is dumped into target/deploy. */
const NO_METAPLEX = existsSync(MPL_SO)
  ? false
  : "Metaplex isn't in target/deploy: `solana program dump -u m metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s target/deploy/mpl_token_metadata.so`";

async function metaWorld(opts: WorldOpts = {}) {
  const w = await world({ shareTokenProgram: TOKEN_2022_PROGRAM_ID, ...opts });
  w.ctx.svm.addProgramFromFile(MPL, MPL_SO);
  return w;
}

const metadataPda = (mint: PublicKey) =>
  PublicKey.findProgramAddressSync([Buffer.from("metadata"), MPL.toBuffer(), mint.toBuffer()], MPL)[0];

const setIx = (w: World, signer: Keypair, name: string, symbol: string, uri = URI, metadata = metadataPda(w.shareMint)) =>
  w.ctx.vault.methods.setShareMetadata(name, symbol, uri).accountsPartial({
    authority: signer.publicKey,
    payer: signer.publicKey,
    basket: w.basket,
    shareMint: w.shareMint,
    metadata,
    tokenProgram: w.shareProgram,
    tokenMetadataProgram: MPL,
    systemProgram: SystemProgram.programId,
    sysvarInstructions: SYSVAR_INSTRUCTIONS,
  });

/** Metaplex's Metadata account: key, update authority, mint, then name, symbol and uri, each
 *  a length-prefixed string padded with NULs. */
function readMetadata(w: World) {
  const acc = w.ctx.svm.getAccount(metadataPda(w.shareMint));
  if (!acc) return null;
  const d = Buffer.from(acc.data);
  let o = 65;
  const str = () => {
    const len = d.readUInt32LE(o);
    const s = d.subarray(o + 4, o + 4 + len).toString("utf8").replace(/\0+$/, "");
    o += 4 + len;
    return s;
  };
  return {
    updateAuthority: new PublicKey(d.subarray(1, 33)),
    mint: new PublicKey(d.subarray(33, 65)),
    name: str(),
    symbol: str(),
    uri: str(),
    lamports: BigInt(acc.lamports),
    rent: w.ctx.svm.minimumBalanceForRentExemption(BigInt(acc.data.length)),
  };
}

const lamports = (w: World, k: PublicKey) => BigInt(w.ctx.svm.getBalance(k) ?? 0n);

describe("share metadata", { skip: NO_METAPLEX }, () => {
  it("the creator names the share; the basket PDA holds the update authority", async () => {
    const w = await metaWorld();
    const before = lamports(w, w.creator.publicKey);
    await rpc(w.ctx, setIx(w, w.creator, "AI Supercycle", "AISC"), [w.creator]);
    const m = readMetadata(w)!;
    assert.equal(m.name, "AI Supercycle");
    assert.equal(m.symbol, "AISC");
    assert.equal(m.uri, URI);
    assert.ok(m.mint.equals(w.shareMint));
    assert.ok(m.updateAuthority.equals(w.basket));
    // The creator paid the rent and Metaplex's fee, which sits in the account on top of rent.
    assert.equal(m.lamports - m.rent, BigInt(METAPLEX_FEE));
    assert.equal(before - lamports(w, w.creator.publicKey), m.lamports);
  });

  it("calling it again updates the metadata, with no fee", async () => {
    const w = await metaWorld();
    await rpc(w.ctx, setIx(w, w.creator, "First", "ONE"), [w.creator]);
    const before = lamports(w, w.creator.publicKey);
    const held = readMetadata(w)!.lamports;
    await rpc(w.ctx, setIx(w, w.creator, "A longer second name", "TWO", URI.replace("v=1", "v=2")), [w.creator]);
    const m = readMetadata(w)!;
    assert.deepEqual([m.name, m.symbol, m.uri], ["A longer second name", "TWO", URI.replace("v=1", "v=2")]);
    assert.equal(m.lamports, held);
    assert.equal(lamports(w, w.creator.publicKey), before);
  });

  it("the protocol authority may name any basket's share, and pays for it", async () => {
    const w = await metaWorld();
    const before = lamports(w, w.creator.publicKey);
    await rpc(w.ctx, setIx(w, w.ctx.payer, "Backfilled", "BACK"));
    assert.equal(readMetadata(w)!.name, "Backfilled");
    assert.equal(lamports(w, w.creator.publicKey), before);
    // ...and update it later, as can the creator.
    await rpc(w.ctx, setIx(w, w.ctx.payer, "Backfilled again", "BACK"));
    await rpc(w.ctx, setIx(w, w.creator, "Creator's name", "BACK"), [w.creator]);
    assert.equal(readMetadata(w)!.name, "Creator's name");
  });

  it("someone else can pay, as the server does on the testnet", async () => {
    const w = await metaWorld();
    const before = lamports(w, w.creator.publicKey);
    await rpc(w.ctx, setIx(w, w.creator, "Paid by server", "SRV").accountsPartial({ payer: w.ctx.payer.publicKey }), [w.creator]);
    assert.equal(readMetadata(w)!.name, "Paid by server");
    assert.equal(lamports(w, w.creator.publicKey), before);
  });

  it("anyone else is refused, on create and on update", async () => {
    const w = await metaWorld();
    const stranger = fund(w.ctx);
    await expectError(rpc(w.ctx, setIx(w, stranger, "Scam", "USDC"), [stranger]), "NotMetadataAuthority");
    await rpc(w.ctx, setIx(w, w.creator, "Real", "REAL"), [w.creator]);
    await expectError(rpc(w.ctx, setIx(w, stranger, "Scam", "USDC"), [stranger]), "NotMetadataAuthority");
    assert.equal(readMetadata(w)!.name, "Real");
  });

  it("nobody can update Metaplex directly: only the basket PDA signs as update authority", async () => {
    const w = await metaWorld();
    await rpc(w.ctx, setIx(w, w.creator, "Real", "REAL"), [w.creator]);
    // Metaplex's UpdateV1, signed by the creator's own key instead of the basket PDA.
    const str = (s: string) => Buffer.concat([Buffer.from(Uint32Array.of(s.length).buffer), Buffer.from(s)]);
    const data = Buffer.concat([Buffer.from([50, 0, 0, 1]), str("Scam"), str("USDC"), str("u"), Buffer.from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])]);
    const keys = [w.creator.publicKey, MPL, MPL, w.shareMint, metadataPda(w.shareMint), MPL, w.creator.publicKey, SystemProgram.programId, SYSVAR_INSTRUCTIONS, MPL, MPL];
    const ix = new TransactionInstruction({
      programId: MPL,
      keys: keys.map((pubkey, i) => ({ pubkey, isSigner: i === 0 || i === 6, isWritable: i === 4 || i === 6 })),
      data,
    });
    await assert.rejects(send(w.ctx, [ix], [w.creator]));
    assert.equal(readMetadata(w)!.name, "Real");
  });

  it("enforces Metaplex's limits, in bytes: name 1-32, symbol 1-10, URI 1-200", async () => {
    const w = await metaWorld();
    const bad: [string, string, string][] = [
      ["x".repeat(33), "SYM", URI],
      ["", "SYM", URI],
      ["é".repeat(17), "SYM", URI], // 34 bytes
      ["Name", "S".repeat(11), URI],
      ["Name", "", URI],
      ["Name", "SYM", "h".repeat(201)],
      ["Name", "SYM", ""],
    ];
    for (const [name, symbol, uri] of bad) {
      await expectError(rpc(w.ctx, setIx(w, w.creator, name, symbol, uri), [w.creator]), "BadMetadata");
    }
    await rpc(w.ctx, setIx(w, w.creator, "x".repeat(32), "S".repeat(10), "h".repeat(200)), [w.creator]);
    assert.equal(readMetadata(w)!.name, "x".repeat(32));
  });

  it("an immutable basket can be named too: it changes nothing a holder owns", async () => {
    const w = await metaWorld({ control: "immutable" });
    await rpc(w.ctx, setIx(w, w.creator, "Fixed Forever", "FIX"), [w.creator]);
    await rpc(w.ctx, setIx(w, w.creator, "Fixed Forever", "FIX", URI.replace("v=1", "v=2")), [w.creator]);
    assert.equal(readMetadata(w)!.uri, URI.replace("v=1", "v=2"));
  });

  it("refuses a metadata account that isn't the share mint's", async () => {
    const w = await metaWorld();
    await expectError(rpc(w.ctx, setIx(w, w.creator, "Name", "SYM", URI, metadataPda(w.mintA)), [w.creator]), "ConstraintSeeds");
  });

  it("refuses a token program that doesn't own the share mint", async () => {
    const w = await metaWorld();
    const ix = setIx(w, w.creator, "Name", "SYM").accountsPartial({ tokenProgram: TOKEN_PROGRAM_ID });
    await expectError(rpc(w.ctx, ix, [w.creator]), "BadParameter");
  });

  it("works on a classic SPL share mint too", async () => {
    const w = await metaWorld({ shareTokenProgram: TOKEN_PROGRAM_ID });
    await rpc(w.ctx, setIx(w, w.creator, "Classic", "CLS"), [w.creator]);
    assert.equal(readMetadata(w)!.symbol, "CLS");
  });

  it("works before the basket is seeded, as the create flow does", async () => {
    const w = await metaWorld({ seed: false });
    await rpc(w.ctx, setIx(w, w.creator, "Unseeded", "NEW"), [w.creator]);
    assert.equal(readMetadata(w)!.name, "Unseeded");
  });
});
