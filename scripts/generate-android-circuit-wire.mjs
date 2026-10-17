// Android's Circuit DTOs come from Rust's ts-rs exports. Keep this deliberately
// limited to records, scalar unions and arrays; fail if the wire adds a new shape.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import ts from 'typescript';

const root = resolve(import.meta.dirname, '..');
const definitions = new Map();
function load(name) {
  if (definitions.has(name)) return;
  const file = ts.createSourceFile(name, readFileSync(resolve(root, 'src/types/generated', `${name}.ts`), 'utf8'), ts.ScriptTarget.Latest, true);
  const alias = file.statements.find(ts.isTypeAliasDeclaration);
  if (!alias) throw new Error(`Missing wire alias: ${name}`);
  definitions.set(name, alias.type);
  function visit(node) {
    if (ts.isTypeReferenceNode(node) && node.typeName.getText(file) !== 'Array') load(node.typeName.getText(file));
    ts.forEachChild(node, visit);
  }
  visit(alias.type);
}
for (const name of ['NodeCircuitRun', 'CircuitAgentOwnership', 'NodeReviewOption', 'StartNodeReviewRequest', 'CheckpointRequest', 'RecoveryRequest', 'ApproveCircuitStepRequest']) load(name);
const scalarUnion = node => ts.isUnionTypeNode(node) && node.types.every(t => ts.isLiteralTypeNode(t) && ts.isStringLiteral(t.literal));
function shape(node) {
  if (node.kind === ts.SyntaxKind.StringKeyword || scalarUnion(node)) return { type: 'String', read: v => `${v}.jsonPrimitive.content`, write: v => `JsonPrimitive(${v})` };
  if (node.kind === ts.SyntaxKind.NumberKeyword) return { type: 'Long', read: v => `${v}.jsonPrimitive.long`, write: v => `JsonPrimitive(${v})` };
  if (node.kind === ts.SyntaxKind.BooleanKeyword) return { type: 'Boolean', read: v => `${v}.jsonPrimitive.boolean`, write: v => `JsonPrimitive(${v})` };
  if (ts.isUnionTypeNode(node)) {
    const nonNull = node.types.filter(t => !(ts.isLiteralTypeNode(t) && t.literal.kind === ts.SyntaxKind.NullKeyword));
    if (nonNull.length !== 1 || nonNull.length === node.types.length) throw new Error('Unsupported wire union');
    const inner = shape(nonNull[0]);
    return { type: `${inner.type}?`, read: v => `${v}.takeUnless { it is JsonNull }?.let { ${inner.read('it')} }`, write: v => `${v}?.let { ${inner.write('it')} } ?: JsonNull` };
  }
  if (ts.isTypeReferenceNode(node)) {
    const name = node.typeName.getText();
    if (name === 'Array') {
      const inner = shape(node.typeArguments[0]);
      return { type: `List<${inner.type}>`, read: v => `${v}.jsonArray.map { ${inner.read('it')} }`, write: v => `JsonArray(${v}.map { ${inner.write('it')} })` };
    }
    if (scalarUnion(definitions.get(name))) return { ...shape(definitions.get(name)), type: name };
    return { type: name, read: v => `${name}.fromJson(${v})`, write: v => `${v}.toJson()` };
  }
  throw new Error(`Unsupported wire shape: ${node.getText()}`);
}
const chunks = ['// Generated from Rust ts-rs bindings by scripts/generate-android-circuit-wire.mjs. Do not edit.', 'package dev.buildmesh.remote.wire', 'import kotlinx.serialization.json.*'];
for (const [name, node] of definitions) {
  if (scalarUnion(node)) { chunks.push(`typealias ${name} = String`); continue; }
  if (!ts.isTypeLiteralNode(node)) throw new Error(`Unsupported wire record: ${name}`);
  const fields = node.members.map(member => {
    if (!ts.isPropertySignature(member)) throw new Error(`Unsupported member: ${name}`);
    const key = member.name.getText();
    const value = shape(member.type);
    return { key, ...value, optional: !!member.questionToken };
  });
  chunks.push(`data class ${name}(\n${fields.map(f => `    val ${f.key}: ${f.type}${f.optional && !f.type.endsWith('?') ? '?' : ''}${f.optional ? ' = null' : ''},`).join('\n')}\n) {
    companion object {
        fun fromJson(value: JsonElement): ${name} {
            val obj = value.jsonObject
            return ${name}(\n${fields.map(f => `                ${f.key} = ${f.optional ? `obj["${f.key}"]?.let { ${f.read('it')} }` : f.read(`obj.getValue("${f.key}")`)},`).join('\n')}
            )
        }
    }
    fun toJson(): JsonObject = buildJsonObject {\n${fields.map(f => `        ${f.optional ? `${f.key}?.let { put("${f.key}", ${f.write('it')}) }` : `put("${f.key}", ${f.write(f.key)})`}`).join('\n')}
    }
}`);
}
const output = resolve(root, process.argv[2] ?? 'android/app/build/generated/circuitWire/dev/buildmesh/remote/wire/CircuitWire.kt');
mkdirSync(dirname(output), { recursive: true });
writeFileSync(output, chunks.join('\n\n') + '\n');
