import test from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import {
  ALLOWED_PRODUCTION_GIT2,
  isSqlStatement,
  productionGit2Paths,
  productionSqlUnderCommands,
  resolveBaseTreeDefinitions,
  rustStringLiterals,
  seamDebtPaths,
  stripCfgTestItems,
} from "./git2-ownership.mjs";

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

test("no production file under commands/ checks a connection out or issues SQL", () => {
  // The second half of the module map's rule (issue #2527): a `#[command]`
  // is an adapter — validate, call one service or `db::` function, map the
  // error to `String`. No policy, no SQL, no `git2` handles. The git2 half
  // is guarded above; this is the SQL half.
  const found = productionSqlUnderCommands(root);
  assert.deepEqual(found, [], `SQL under commands/:\n${found.join("\n")}`);
});

test("the SQL guard reads literals, not prose", () => {
  // A comment that names a statement is prose; a statement in a command is
  // not. The scanner has to tell them apart, or the guard either misses a
  // real statement or cries wolf over a doc comment.
  assert.deepEqual(rustStringLiterals('// "UPDATE meshes SET name = ?1"'), []);
  assert.deepEqual(rustStringLiterals('/* "SELECT 1" */'), []);
  assert.deepEqual(rustStringLiterals('let label = "UPDATE meshes SET name = ?1";'), [
    "UPDATE meshes SET name = ?1",
  ]);
  // A char literal is not a string delimiter.
  assert.deepEqual(rustStringLiterals("let quote = '\"';"), []);
  // A raw string is one literal, hashes included.
  assert.deepEqual(rustStringLiterals('let sql = r#"SELECT "x" FROM t"#;'), ['SELECT "x" FROM t']);
});

test("the SQL rule reads a statement, not an English sentence", () => {
  // The repository writes its statements with an UPPERCASE verb, and the
  // literals that open with a lowercase one are prose or a git ref name.
  // "Select a built-in Review Blueprint to copy." is a user-facing message
  // the crate already carries (`db/error.rs`), so a rule that read it as a
  // statement would break the build on a legitimate message.
  assert.equal(isSqlStatement("UPDATE meshes SET name = ?1 WHERE id = ?2"), true);
  assert.equal(isSqlStatement("SELECT value FROM app_settings"), true);
  assert.equal(isSqlStatement("INSERT INTO meshes (id) VALUES (?1)"), true);
  // A multi-line statement puts its verb on its own line. The crate's own
  // `db/mod.rs` carries `CREATE TABLE IF NOT EXISTS …` literals shaped this
  // way, so a rule that stopped at the newline would miss every one of them.
  assert.equal(isSqlStatement("\n            CREATE TABLE IF NOT EXISTS meshes (\n                id INTEGER PRIMARY KEY\n            )"), true);
  assert.equal(isSqlStatement("Select a built-in Review Blueprint to copy."), false);
  assert.equal(isSqlStatement("Delete from the list failed"), false);
  assert.equal(isSqlStatement("Update failed"), false);
  assert.equal(isSqlStatement("update-index"), false);
});

test("resolve_base_tree has exactly one definition in the crate", () => {
  // CONTEXT.md names this seam for the Node Change Set: the file list and
  // the per-file diff both hang off it, so a second definition would let
  // them disagree about what "changed since the branch" means.
  assert.deepEqual(resolveBaseTreeDefinitions(root), ["git/diff.rs"]);
});

test("the SQL guard reports a command that owns a connection or a statement", () => {
  // A guard that cannot fail is not a guard. This fixture gives it the two
  // shapes it has to catch, and a clean command to leave alone.
  const fixture = mkdtempSync(join(tmpdir(), "buildmesh-sql-guard-"));
  const commands = join(fixture, "src-tauri", "src", "commands");
  mkdirSync(commands, { recursive: true });
  writeFileSync(
    join(commands, "checkout.rs"),
    "pub fn read() {\n    let db = db::read_conn();\n    db.query_row(\"SELECT value FROM app_settings\", [], |r| r.get(0))\n}\n",
  );
  writeFileSync(
    join(commands, "statement.rs"),
    'pub fn write() {\n    db::write_conn().execute("UPDATE meshes SET name = ?1", []);\n}\n',
  );
  writeFileSync(
    join(commands, "clean.rs"),
    'pub fn read() {\n    crate::db::get_mesh_by_id(1).map(|mesh| mesh.name)\n}\n',
  );
  writeFileSync(
    join(commands, "prose.rs"),
    'pub fn message() -> String {\n    "Select a built-in Review Blueprint to copy.".to_string()\n}\n',
  );
  // A multi-line statement puts its verb on its own line. This one carries
  // no connection checkout, so the only reason it can be reported is the
  // statement itself.
  writeFileSync(
    join(commands, "schema.rs"),
    'pub const SCHEMA: &str = "\n            CREATE TABLE IF NOT EXISTS meshes (\n                id INTEGER PRIMARY KEY\n            )";\n',
  );
  try {
    const found = productionSqlUnderCommands(fixture);
    assert.deepEqual(found, [
      "commands/checkout.rs (checks a connection out; issues a SQL statement)",
      "commands/schema.rs (issues a SQL statement)",
      "commands/statement.rs (checks a connection out; issues a SQL statement)",
    ]);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
});
