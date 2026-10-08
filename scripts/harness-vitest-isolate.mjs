import { startVitest } from 'vitest/node';

const [file, pattern, report] = process.argv.slice(2);
if (!file || !pattern || !report) throw new Error('Usage: harness-vitest-isolate.mjs <file> <anchored test pattern> <report>');

// CLI file filters are substrings: .test.ts also selects .test.tsx. The API's
// include override constrains discovery to the failed file, retaining its config.
const context = await startVitest('test', [file], {
  run: true,
  include: [file],
  testNamePattern: pattern,
  maxWorkers: 1,
  fileParallelism: false,
  reporters: ['default', 'json'],
  outputFile: { json: report },
});
if (context) await context.close();
else process.exitCode = 1;
