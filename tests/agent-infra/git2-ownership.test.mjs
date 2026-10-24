import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { ALLOWED_PRODUCTION_GIT2, productionGit2Paths, seamDebtPaths, stripCfgTestItems } from "./git2-ownership.mjs";

const root = fileURLToPath(new URL("../..", import.meta.url));

test("no production file outside git/ names git2::", () => {
  const found = productionGit2Paths(root);
  const allowed = [...ALLOWED_PRODUCTION_GIT2].sort();
  assert.deepEqual(found, allowed, `production git2 outside git/:\n${found.join("\n")}`);
  assert.deepEqual(seamDebtPaths(root).sort(), allowed);
});

test("the seam-debt list in the module map is empty", () => {
  // The module map is the human-readable half of the same rule the tree
  // walk enforces. A stale entry there would send the next reader looking
  // for debt that no longer exists.
  assert.deepEqual(seamDebtPaths(root), []);
});

test("an unbalanced cfg(test) module does not hide later production functions", () => {
  const stripped = stripCfgTestItems(readFileSync(join(root, "src-tauri", "src", "db", "agent_node.rs"), "utf8"));
  assert.match(stripped, /pub fn create_agent_node/);
  assert.match(stripped, /pub fn update_agent_node_positions_batch/);
});
