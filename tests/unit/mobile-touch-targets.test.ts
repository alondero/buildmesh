/**
 * Structural regression pins for issue #1261. Cheap "source contains X"
 * checks that catch an accidental regression on the user-facing pieces
 * without booting the full app:
 *
 *   * `.qk-btn` keeps the 44px hit-target floor (Apple HIG / Material 48dp).
 *     An Esc / ^C mis-tap on this strip costs real time with live agents
 *     — silent regressions to the floor are easy if a future tweak
 *     trims padding without thinking about touch targets.
 *   * `chip-btn` carries the comment that documents WHY it stays below
 *     the 44px floor — so the next person who reads the rule doesn't
 *     "fix" it and re-introduce the regression.
 *   * `playwright.config.base.ts` exists and is imported by BOTH
 *     Playwright configs. Catches a future PR that edits one config
 *     without realising they should both share the timeouts.
 *   * `playwright.config.ts`'s `chromium` project has a `webServer`
 *     block — silent removal would make `npm run test:e2e` fail
 *     instantly with the same undocumented precondition the original
 *     bug had.
 *
 * These run as Vitest unit tests so they fail in CI without needing
 * Playwright's browser binary or the Tauri exe.
 */
import { describe, it, expect } from "vitest";
import { readFileSync, existsSync } from "fs";
import { resolve } from "path";

const repoRoot = resolve(__dirname, "..", "..");

describe("issue #1261 structural pins", () => {
  it(".qk-btn keeps the 44px touch-target floor", () => {
    const css = readFileSync(
      resolve(repoRoot, "src/mobile/styles.css"),
      "utf8",
    );
    const qkRule = css.match(/\.qk-btn\s*\{[^}]*\}/);
    expect(qkRule, ".qk-btn rule must exist").toBeTruthy();
    expect(qkRule![0]).toMatch(/min-height:\s*44px/);
  });

  it(".chip-btn documents why it stays below the 44px floor", () => {
    const css = readFileSync(
      resolve(repoRoot, "src/mobile/styles.css"),
      "utf8",
    );
    // Look for the explanatory comment block immediately above the
    // `.chip-btn` rule — guards against the comment being dropped in a
    // reformat AND against the rule being "fixed" later. Match on
    // `.chip-btn {` (with the brace) so we land on the rule itself —
    // `indexOf(".chip-btn")` would otherwise hit the `.chip-btn` token
    // inside the comment's backticks and slice from the wrong place.
    // The comment is multi-line; a 1500-char window covers it without
    // relying on exact line count.
    const idx = css.indexOf(".chip-btn {");
    expect(idx, ".chip-btn rule must exist").toBeGreaterThan(-1);
    const before = css.slice(Math.max(0, idx - 1500), idx);
    expect(before).toMatch(/issue #1261/);
    expect(before).toMatch(/44px HIG/);
  });

  it("playwright.config.base.ts exists and is shared by both configs", () => {
    const basePath = resolve(repoRoot, "playwright.config.base.ts");
    expect(existsSync(basePath), "base config file must exist").toBe(true);
    const defaultCfg = readFileSync(
      resolve(repoRoot, "playwright.config.ts"),
      "utf8",
    );
    const standaloneCfg = readFileSync(
      resolve(repoRoot, "playwright.config.standalone.ts"),
      "utf8",
    );
    expect(defaultCfg).toMatch(/from ['"]\.\/playwright\.config\.base['"]/);
    expect(standaloneCfg).toMatch(
      /from ['"]\.\/playwright\.config\.base['"]/,
    );
    // Sanity: the base actually carries the lenient shared timeouts so
    // a future PR that drops them to the old divergent defaults trips
    // the regression net here.
    const baseCfg = readFileSync(basePath, "utf8");
    expect(baseCfg).toMatch(/timeout:\s*60000/);
    expect(baseCfg).toMatch(/timeout:\s*15000/);
  });

  it("playwright.config.ts chromium project has webServer (issue #1261)", () => {
    const cfg = readFileSync(
      resolve(repoRoot, "playwright.config.ts"),
      "utf8",
    );
    // The chromium project is the FIRST entry in the projects array.
    // Anchor on `name: 'chromium'` and read forward to the next
    // project's `name:` line — that boundary is robust to intervening
    // comments and whitespace (which the old `},\s*\{` regex tripped
    // over). Assert webServer is wired with reuseExistingServer so a
    // user with `npm run dev` already up doesn't pay a second boot.
    const chromiumBlock = cfg.match(
      /name:\s*['"]chromium['"][\s\S]*?(?=name:\s*['"]verify-smoke['"])/,
    );
    expect(chromiumBlock, "chromium project block must exist").toBeTruthy();
    expect(chromiumBlock![0]).toMatch(/webServer:/);
    expect(chromiumBlock![0]).toMatch(/reuseExistingServer:\s*true/);
  });
});
