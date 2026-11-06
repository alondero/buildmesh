// Shared by the Vitest parser cases and the tree walk in git2-ownership.test.mjs.
// The walk stays out of tests/unit: frontend-tests reuses a Rust-only edit, and
// a scan of every Rust file would make that reuse unsound.

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, sep } from "node:path";

/**
 * Paths relative to src-tauri/src that may still name `git2::` outside
 * `git/`. Empty since issue #2526: the raw `Repository` handle no longer
 * leaves the `git` module, so no production file outside it names `git2::`.
 *
 * Keep in step with the seam-debt list in docs/development/module-map.md —
 * the ownership test asserts the two agree.
 */
export const ALLOWED_PRODUCTION_GIT2 = [];

function isCfgTestAttr(line) {
  // `test` as its own predicate, including inside all(...) and any(...).
  // `not(test)` is production. A `)` inside a feature literal must not hide
  // a later `test` predicate.
  return /^\s*#\[cfg\((?:test\b|(?:all|any)\([^\]]*?(?<!not\()test\b)/.test(line);
}

/** Drop items attributed with #[cfg(test)], so fixtures are not production use. */
export function stripCfgTestItems(source) {
  const lines = source.split(/\r?\n/);
  const kept = [];
  let i = 0;
  while (i < lines.length) {
    if (!isCfgTestAttr(lines[i])) {
      kept.push(lines[i]);
      i += 1;
      continue;
    }
    // Only a whole-line attribute is skipped. An inline `#[cfg(test)] fn`
    // is the item; skipping that line would delete the next production line.
    // A comment between the attribute and the item belongs to the attribute's
    // header — the `// allow-test-in-production: <reason>` escape hatch sits
    // there — so it is skipped too. Otherwise a semicolon inside that reason
    // ends the item early and exposes the fixture below as production use.
    let j = i;
    for (;;) {
      if (j >= lines.length) break;
      const line = lines[j];
      const attribute = line.match(/^\s*#\[[^\]]*\](.*)$/);
      if (attribute) {
        const rest = attribute[1].trim();
        // A trailing comment is still the attribute's header; anything else
        // after the bracket is the item itself.
        if (rest !== "" && !rest.startsWith("//")) break;
        j += 1;
        continue;
      }
      if (lines[j].trim() === "" || /^\s*\/\//.test(lines[j])) {
        j += 1;
        continue;
      }
      break;
    }
    if (j >= lines.length) {
      kept.push(...lines.slice(i));
      break;
    }
    let depth = 0;
    let seenBrace = false;
    let closed = false;
    let k = j;
    for (; k < lines.length; k += 1) {
      const line = lines[k];
      for (const ch of line) {
        if (ch === "{") {
          depth += 1;
          seenBrace = true;
        } else if (ch === "}") {
          depth -= 1;
        }
      }
      if (!seenBrace && line.includes(";")) {
        closed = true;
        k += 1;
        break;
      }
      if (seenBrace && depth <= 0) {
        closed = true;
        k += 1;
        break;
      }
    }
    // Braces that never return to zero used to drop every later line. Keep
    // the tail so production git2 use after a broken count stays visible.
    if (!closed) kept.push(...lines.slice(i, k));
    i = k;
  }
  return kept.join("\n");
}

function walk(dir, acc = []) {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) walk(full, acc);
    else if (entry.endsWith(".rs")) acc.push(full);
  }
  return acc;
}

function isTestOnlyFile(rel) {
  return rel.endsWith("_tests.rs") || rel.endsWith("/tests.rs") || rel === "tests.rs";
}

export function productionGit2Paths(root) {
  const src = join(root, "src-tauri", "src");
  return walk(src)
    .map((full) => full.slice(src.length + 1).split(sep).join("/"))
    .filter((rel) => !rel.startsWith("git/") && !isTestOnlyFile(rel))
    .filter((rel) => stripCfgTestItems(readFileSync(join(src, ...rel.split("/")), "utf8")).includes("git2::"))
    .sort();
}

export function seamDebtPaths(root) {
  const map = readFileSync(join(root, "docs", "development", "module-map.md"), "utf8");
  const section = map.split(/^## Seam debt\s*$/m)[1]?.split(/^## /m)[0] ?? "";
  return [...section.matchAll(/`([^`\s]*\/[^`]+\.rs)`/g)].map((match) => match[1]);
}

// ---------------------------------------------------------------------------
// The SQL rule (issue #2527)
// ---------------------------------------------------------------------------

/**
 * A SQL statement, as it appears at the start of a string literal.
 *
 * The guard reads *literals*, not the whole file: a comment that names a
 * statement is prose, and a statement that a command issues is not. The
 * repository's own SQL always opens the literal with the verb
 * (`"UPDATE meshes SET …"`, `"INSERT INTO … "`, `"SELECT … FROM …"`), so
 * anchoring on the verb is both precise and cheap.
 *
 * The verb is matched in UPPERCASE, because that is how the repository
 * writes its statements, and the case-insensitive reading was a false
 * positive generator: the crate's SQL-shaped literals that start with a
 * lowercase verb are prose (`"Select a built-in Review Blueprint to copy."`,
 * `"update the PR"`) or a git ref name (`"update-index"`). A guard that
 * fired on the repository's own English would cry wolf on the first command
 * that carried a user-facing message, and a guard that cries wolf is a
 * guard somebody turns off.
 *
 * The leading whitespace is allowed, because a multi-line statement is
 * written with its verb on its own line — the crate's own `db/mod.rs`
 * carries `CREATE TABLE IF NOT EXISTS …` literals shaped that way, and a rule
 * that stopped at the newline would miss every one of them.
 *
 * The rule's limits, so a future reader knows what it does not watch: a
 * connection taken through an aliased `use` (`use … as …`) is not seen, and
 * neither is a statement whose verb is not the literal's first token.
 */
const SQL_STATEMENT =
  /^\s*(?:SELECT\b|INSERT\s+INTO\b|REPLACE\s+INTO\b|UPDATE\b|DELETE\s+FROM\b|CREATE\s+(?:TABLE|INDEX|UNIQUE|VIEW|TRIGGER)\b|DROP\s+(?:TABLE|INDEX|VIEW|TRIGGER)\b|ALTER\s+TABLE\b|PRAGMA\b)/;

/**
 * Scan a Rust source file for its string literals and its code.
 *
 * A hand-written scan rather than a regex: the guard must not read a
 * comment's quoted SQL as a statement, must not read a comment that names
 * `db::read_conn()` as a call, and must not read a `'c'` char literal as a
 * quote. It walks the source the way rustc does — line comments, block
 * comments (nested), string literals (with escapes), raw string literals,
 * and char literals.
 *
 * `code` is the source with every comment and every string literal blanked
 * out (newlines kept, so line numbers survive), so a caller can look for
 * *calls* in it. `literals` holds the string literals themselves, so a
 * caller can look for *statements* in them.
 */
export function scanRust(source) {
  const literals = [];
  let code = "";
  let i = 0;
  const blank = (text) => text.replace(/[^\n]/g, " ");
  while (i < source.length) {
    const rest = source.slice(i);
    if (rest.startsWith("//")) {
      const end = source.indexOf("\n", i);
      const stop = end === -1 ? source.length : end;
      code += blank(source.slice(i, stop));
      i = stop;
      continue;
    }
    if (rest.startsWith("/*")) {
      let depth = 1;
      let j = i + 2;
      while (j < source.length && depth) {
        if (source.startsWith("/*", j)) {
          depth += 1;
          j += 2;
          continue;
        }
        if (source.startsWith("*/", j)) {
          depth -= 1;
          j += 2;
          continue;
        }
        j += 1;
      }
      code += blank(source.slice(i, j));
      i = j;
      continue;
    }
    if (rest.startsWith("r") && !/[A-Za-z0-9_]/.test(source[i - 1] ?? " ")) {
      const raw = rest.match(/^r(#*)"/);
      if (raw) {
        const close = `"${raw[1]}`;
        const end = source.indexOf(close, i + raw[0].length);
        if (end === -1) {
          code += blank(source.slice(i));
          i = source.length;
          continue;
        }
        literals.push(source.slice(i + raw[0].length, end));
        code += blank(source.slice(i, end + close.length));
        i = end + close.length;
        continue;
      }
    }
    if (rest.startsWith('"')) {
      let j = i + 1;
      let value = "";
      while (j < source.length) {
        const ch = source[j];
        if (ch === "\\") {
          value += source.slice(j, j + 2);
          j += 2;
          continue;
        }
        if (ch === '"') break;
        value += ch;
        j += 1;
      }
      literals.push(value);
      code += blank(source.slice(i, j + 1));
      i = j + 1;
      continue;
    }
    // A char literal, so its quote is not a string delimiter. A lifetime
    // (`&'a str`) is not a char literal and falls through.
    if (rest.startsWith("'")) {
      const literal = rest.match(/^'(?:\\.|[^\\'])'/);
      if (literal) {
        code += blank(literal[0]);
        i += literal[0].length;
        continue;
      }
    }
    code += source[i];
    i += 1;
  }
  return { code, literals };
}

/**
 * Whether a string literal opens like a SQL statement.
 *
 * @see SQL_STATEMENT
 */
export function isSqlStatement(literal) {
  return SQL_STATEMENT.test(literal);
}

/**
 * The string literals of a Rust source file.
 *
 * @see scanRust
 */
export function rustStringLiterals(source) {
  return scanRust(source).literals;
}

/**
 * The definitions of the crate's one baseline seam.
 *
 * CONTEXT.md names `resolve_base_tree` as *the* seam the Node Change Set
 * hangs off. Two definitions would let the two readers — the file list and
 * the per-file diff — disagree about what "changed since the branch" means,
 * which is the disagreement issue #2527 closed.
 */
export function resolveBaseTreeDefinitions(root) {
  const src = join(root, "src-tauri", "src");
  return walk(src)
    .filter((file) => /\bfn resolve_base_tree\b/.test(readFileSync(file, "utf8")))
    .map((file) => file.slice(src.length + 1).split(sep).join("/"))
    .sort();
}

/**
 * The production files under `commands/` that check a connection out of the
 * pool or carry a statement of their own.
 *
 * `db::write_conn()` / `db::read_conn()` are the only ways a command reaches
 * a connection, and a statement in a command is a statement in the wrong
 * layer. Both belong to `db::` (issue #2527).
 */
export function productionSqlUnderCommands(root) {
  const src = join(root, "src-tauri", "src");
  const found = [];
  for (const rel of walk(join(src, "commands"))
    .map((full) => full.slice(src.length + 1).split(sep).join("/"))
    .filter((rel) => !isTestOnlyFile(rel))) {
    const source = stripCfgTestItems(readFileSync(join(src, ...rel.split("/")), "utf8"));
    const { code, literals } = scanRust(source);
    const reasons = [];
    if (/\b(?:write_conn|read_conn)\s*\(/.test(code)) reasons.push("checks a connection out");
    if (literals.some((literal) => isSqlStatement(literal))) {
      reasons.push("issues a SQL statement");
    }
    if (reasons.length) found.push(`${rel} (${reasons.join("; ")})`);
  }
  return found.sort();
}
