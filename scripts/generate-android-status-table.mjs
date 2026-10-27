// The Android client's status vocabulary is generated from the one table the
// desktop and the mobile SPA share (`STATUS_CONFIG` in `src/lib/status.ts`).
//
// A hand-copied mirror drifts silently and, worse, fails soft: a status this
// build does not know used to render as a muted grey dot with a generic label
// instead of saying it did not recognise the status (issue #2612).
//
// The generated file is committed, and `tests/unit/android-token-mirror.test.ts`
// recomputes it from the source, so adding a status without regenerating fails
// a test instead of shipping a vocabulary the phone has not caught up with.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { pathToFileURL } from 'node:url';
import ts from 'typescript';

const root = resolve(import.meta.dirname, '..');

export const STATUS_SOURCE = resolve(root, 'src/lib/status.ts');
export const STATUS_TABLE = resolve(
  root,
  'android/app/src/main/java/dev/buildmesh/remote/MeshStatusTable.kt',
);

/** The only animation class STATUS_CONFIG uses today: a status that pulses. */
const PULSE_CLASS = 'animate-pulse-fast';

/**
 * One row of the shared status vocabulary, as the Android client reads it.
 *
 * `pulsing` comes from the `animate-pulse-fast` class STATUS_CONFIG carries on
 * a status, because the phone's glyph pulses on those statuses and a
 * hand-written set of their names is one more copy to drift.
 *
 * @typedef {{status: string, label: string, hex: string, glyph: string, dot: string, pulsing: boolean}} StatusRow
 */

/** Parse `STATUS_CONFIG` out of `src/lib/status.ts` with the TypeScript parser.
 * @returns {StatusRow[]}
 */
export function statusRows(source) {
  const file = ts.createSourceFile('status.ts', source, ts.ScriptTarget.Latest, true);
  let table;
  for (const statement of file.statements) {
    if (!ts.isVariableStatement(statement)) continue;
    for (const declaration of statement.declarationList.declarations) {
      if (ts.isIdentifier(declaration.name) && declaration.name.text === 'STATUS_CONFIG') {
        table = declaration.initializer;
      }
    }
  }
  if (!table) throw new Error('STATUS_CONFIG is not declared in src/lib/status.ts');
  const object = ts.isAsExpression(table) ? table.expression : table;
  if (!ts.isObjectLiteralExpression(object)) throw new Error('STATUS_CONFIG is not an object literal');

  return object.properties.map(property => {
    if (!ts.isPropertyAssignment(property) || !ts.isIdentifier(property.name)) {
      throw new Error('STATUS_CONFIG holds a status this generator cannot name');
    }
    const status = property.name.text;
    const value = property.initializer;
    if (!ts.isObjectLiteralExpression(value)) throw new Error(`STATUS_CONFIG.${status} is not an object literal`);
    const fields = new Map();
    for (const field of value.properties) {
      if (!ts.isPropertyAssignment(field) || !ts.isIdentifier(field.name)) {
        throw new Error(`STATUS_CONFIG.${status} holds a field this generator cannot name`);
      }
      if (!ts.isStringLiteral(field.initializer)) {
        throw new Error(`STATUS_CONFIG.${status}.${field.name.text} is not a string literal`);
      }
      fields.set(field.name.text, field.initializer.text);
    }
    const required = ['label', 'hex', 'glyph', 'dot'];
    for (const key of required) {
      if (!fields.has(key)) throw new Error(`STATUS_CONFIG.${status} is missing ${key}`);
    }
    const colour = fields.get('color') ?? '';
    const background = fields.get('bgColor') ?? '';
    // An animation class other than `animate-pulse-fast` would drop the pulse
    // without saying so, so the generator refuses to read one.
    for (const className of [...colour.split(/\s+/), ...background.split(/\s+/)]) {
      if (className.startsWith('animate-') && className !== PULSE_CLASS) {
        throw new Error(`STATUS_CONFIG.${status} uses the unknown animation class ${className}`);
      }
    }
    return {
      status,
      label: fields.get('label'),
      hex: fields.get('hex'),
      glyph: fields.get('glyph'),
      dot: fields.get('dot'),
      pulsing: colour.includes(PULSE_CLASS) || background.includes(PULSE_CLASS),
    };
  });
}

/** `#7a8492` → `0xff7a8492L`, the ARGB value Compose's `Color(Long)` reads. */
function argb(hex) {
  const match = /^#([0-9a-f]{6})$/i.exec(hex);
  if (!match) throw new Error(`Not a 6-digit status hex: ${hex}`);
  return `0xff${match[1].toLowerCase()}L`;
}

/**
 * Kotlin string literal for a label or glyph.
 *
 * Kotlin reads `$` as a template start and rejects a raw newline inside a
 * string, so both are escaped rather than emitted verbatim.
 */
function literal(value) {
  const escaped = value
    .replace(/\\/g, '\\\\')
    .replace(/"/g, '\\"')
    .replace(/\$/g, '\\$')
    .replace(/\r/g, '\\r')
    .replace(/\n/g, '\\n');
  return `"${escaped}"`;
}

/** The generated Kotlin source for the status table. */
export function androidStatusTable(source) {
  const rows = statusRows(source);
  const entries = rows
    .map(row => `        MeshStatus(${literal(row.status)}, ${literal(row.label)}, ${argb(row.hex)}, ${literal(row.glyph)}, ${literal(row.dot)}, ${row.pulsing}),`)
    .join('\n');
  const pulsing = rows.filter(row => row.pulsing).map(row => row.status);

  return `// Generated from src/lib/status.ts by scripts/generate-android-status-table.mjs.
// Do not edit: run \`npm run android:status-table\` and commit the result.
package dev.buildmesh.remote

/**
 * One row of the agent-status vocabulary the desktop and the mobile SPA share
 * (\`STATUS_CONFIG\` in \`src/lib/status.ts\`).
 *
 * @property hex the ARGB value Compose's \`Color(Long)\` reads.
 * @property glyph the desktop's circle shape; the phone draws the same shapes.
 * @property dot the text glyph the mobile SPA draws inline.
 * @property pulsing whether the status pulses, as STATUS_CONFIG's
 *   \`animate-pulse-fast\` class marks it.
 */
data class MeshStatus(
    val status: String,
    val label: String,
    val hex: Long,
    val glyph: String,
    val dot: String,
    val pulsing: Boolean,
)

/**
 * The statuses this build knows.
 *
 * A status the desktop reports that is missing here is an explicit gap rather
 * than a muted dot: [resolve] answers the [Unknown] marker, so the phone says
 * the status is unknown instead of showing a healthy-looking grey circle.
 */
object MeshStatusTable {
    val entries: List<MeshStatus> = listOf(
${entries}
    )

    /** The marker for a status this build does not know. */
    val Unknown = MeshStatus("unknown", "Unknown status", 0xffef4444L, "cross", "?", false)

    private val byStatus: Map<String, MeshStatus> = entries.associateBy { it.status }

    /** The statuses that pulse, as STATUS_CONFIG marks them. */
    val pulsing: Set<String> = setOf(${pulsing.map(literal).join(', ')})

    /** The row for [status], or null when this build does not know that status. */
    fun find(status: String): MeshStatus? = byStatus[status]

    /** The row for [status], or the [Unknown] marker when this build does not know it. */
    fun resolve(status: String): MeshStatus = byStatus[status] ?: Unknown
}
`;
}

function main(argv) {
  const source = readFileSync(STATUS_SOURCE, 'utf8');
  const generated = androidStatusTable(source);
  const check = argv.includes('--check');
  if (check) {
    let current = '';
    try {
      current = readFileSync(STATUS_TABLE, 'utf8');
    } catch {
      throw new Error(`${STATUS_TABLE} is missing. Run \`npm run android:status-table\`.`);
    }
    if (current !== generated) {
      throw new Error('The committed Android status table is stale. Run `npm run android:status-table` and commit the result.');
    }
    process.stdout.write('Android status table is current.\n');
    return;
  }
  mkdirSync(dirname(STATUS_TABLE), { recursive: true });
  writeFileSync(STATUS_TABLE, generated);
  process.stdout.write(`Wrote ${STATUS_TABLE}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
