import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

import { PREREQUISITES_URL } from '../../src/lib/urls';

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '../..');

describe('user-facing URLs', () => {
  it('keeps the setup link anchored to a real README heading', () => {
    const readme = readFileSync(join(repoRoot, 'README.md'), 'utf8');
    const anchor = new URL(PREREQUISITES_URL).hash.slice(1);
    const headings = [...readme.matchAll(/^#{1,6}\s+(.+?)\s*#*\s*$/gm)].map((match) => match[1]);
    const githubAnchor = (heading: string) => heading
      .toLowerCase()
      .trim()
      .replace(/[`*_~]/g, '')
      .replace(/[^\p{L}\p{N}\s-]/gu, '')
      .replace(/\s+/g, '-');

    expect(headings.map(githubAnchor)).toContain(anchor);
  });
});
