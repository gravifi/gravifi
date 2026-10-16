import { test } from "node:test";
import assert from "node:assert/strict";
import { world, SEED_SHARES, UNITS_A, UNITS_B } from "./fixtures.ts";
import { balance, supply } from "./helpers.ts";

test("smoke: a basket can be created and seeded", async () => {
  const w = await world();
  assert.equal(supply(w.ctx, w.shareMint), SEED_SHARES);
  assert.equal(balance(w.ctx, w.vaultA), (UNITS_A * SEED_SHARES) / 1_000_000_000n);
  assert.equal(balance(w.ctx, w.vaultB), (UNITS_B * SEED_SHARES) / 1_000_000_000n);
});
