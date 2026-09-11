import { lstatSync, mkdirSync, readdirSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = process.argv.includes('--check');
const differences = [];

function stat(path) {
  try { return lstatSync(path); } catch (error) {
    if (error.code === 'ENOENT') return null;
    throw error;
  }
}

function mirror(source, destination) {
  const sourceStat = lstatSync(source);
  let destinationStat = stat(destination);
  if (sourceStat.isSymbolicLink()) throw new Error(`Canonical context must be a regular file or directory: ${source}`);
  if (destinationStat?.isSymbolicLink() || (sourceStat.isDirectory() && destinationStat?.isFile())) {
    differences.push(destination);
    if (check) return;
    const expected = sourceStat.isDirectory() ? '../.claude/skills' : 'CLAUDE.md';
    if (!destinationStat.isSymbolicLink() && readFileSync(destination, 'utf8').trim() !== expected) {
      throw new Error(`Refusing to replace unexpected context file: ${destination}`);
    }
    unlinkSync(destination);
    destinationStat = null;
  }
  if (sourceStat.isDirectory()) {
    if (!destinationStat && !check) mkdirSync(destination, { recursive: true });
    const names = readdirSync(source);
    if (destinationStat?.isDirectory()) {
      for (const name of readdirSync(destination)) {
        if (!names.includes(name)) throw new Error(`Remove obsolete mirror explicitly: ${join(destination, name)}`);
      }
    }
    for (const name of names) mirror(join(source, name), join(destination, name));
    return;
  }
  const content = readFileSync(source);
  if (!destinationStat?.isFile() || !readFileSync(destination).equals(content)) {
    differences.push(destination);
    if (!check) {
      mkdirSync(dirname(destination), { recursive: true });
      writeFileSync(destination, content);
    }
  }
}

mirror(join(root, 'CLAUDE.md'), join(root, 'AGENTS.md'));
mirror(join(root, '.claude/skills'), join(root, '.agents/skills'));
if (check && differences.length) {
  console.error('AI context mirrors differ. Run npm run sync:ai-context and commit the generated files.');
  process.exitCode = 1;
} else {
  console.log(check ? 'AI context mirrors match.' : 'AI context mirrors refreshed.');
}
