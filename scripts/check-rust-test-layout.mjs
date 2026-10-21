// Preserve offsets while hiding comments and literals: fixture strings often
// contain Rust attributes and unmatched braces that are data, not items.
function maskLiterals(source) {
  let masked = '';
  let at = 0;
  const comments = [];
  while (at < source.length) {
    const rest = source.slice(at);
    let length = 0;
    if (rest.startsWith('//')) {
      length = rest.indexOf('\n') === -1 ? rest.length : rest.indexOf('\n');
      comments.push({ start: at, text: rest.slice(0, length) });
    }
    else if (rest.startsWith('/*')) {
      let depth = 1;
      length = 2;
      while (length < rest.length && depth) {
        if (rest.startsWith('/*', length)) { depth++; length += 2; }
        else if (rest.startsWith('*/', length)) { depth--; length += 2; }
        else length++;
      }
    } else {
      const raw = /^(?:b|c)?r(#{0,255})"/.exec(rest);
      const string = /^(?:b|c)?"/.exec(rest);
      const character = /^(?:b)?'(?:\\(?:u\{[\da-fA-F_]+\}|x[\da-fA-F]{2}|[^\n])|[^'\\\r\n])'/.exec(rest);
      if (raw) {
        const end = rest.indexOf(`"${raw[1]}`, raw[0].length);
        length = end === -1 ? rest.length : end + raw[1].length + 1;
      } else if (string) {
        length = string[0].length;
        while (length < rest.length) {
          if (rest[length] === '\\') length += 2;
          else if (rest[length++] === '"') break;
        }
      } else if (character) length = character[0].length;
    }
    if (length) { masked += rest.slice(0, length).replace(/[^\n]/g, ' '); at += length; }
    else { masked += source[at]; at++; }
  }
  return { masked, comments };
}

function itemEnd(source, start) {
  let depth = 0;
  let brackets = 0;
  let parentheses = 0;
  let body = false;
  for (let at = start; at < source.length; at++) {
    const ch = source[at];
    if (ch === '[') brackets++;
    else if (ch === ']') brackets--;
    else if (ch === '(') parentheses++;
    else if (ch === ')') parentheses--;
    else if (ch === '{') { depth++; body = true; }
    else if (ch === '}' && --depth === 0 && body && brackets === 0 && parentheses === 0) return at + 1;
    else if (ch === ';' && depth === 0 && brackets === 0 && parentheses === 0) return at + 1;
  }
  return source.length;
}

function requiresTest(predicate) {
  predicate = predicate.trim();
  if (predicate === 'test') return true;
  const compound = /^(all|any)\s*\(([\s\S]*)\)$/.exec(predicate);
  if (!compound) return false;
  const terms = [];
  let depth = 0;
  let start = 0;
  for (let at = 0; at <= compound[2].length; at++) {
    const ch = compound[2][at];
    if (ch === '(') depth++;
    else if (ch === ')') depth--;
    else if ((ch === ',' && depth === 0) || at === compound[2].length) {
      const term = compound[2].slice(start, at).trim();
      if (term) terms.push(requiresTest(term));
      start = at + 1;
    }
  }
  return compound[1] === 'all' ? terms.some(Boolean) : terms.length > 0 && terms.every(Boolean);
}

export function rustTestLayoutViolations(file, source, addedLines) {
  file = file.replaceAll('\\', '/');
  if (!file.startsWith('src-tauri/src/') || !file.endsWith('.rs') || /(?:^|\/)tests(?:\/|\.rs$)|_tests\.rs$/.test(file)) return [];
  const { masked, comments } = maskLiterals(source);
  const attributes = /#\s*\[/g;
  const failures = [];
  let match;
  while ((match = attributes.exec(masked))) {
    const start = match.index;
    let cursor = start;
    const group = [];
    // Attributes can precede cfg(test) or follow it, including #[path].
    while (/^#\s*\[/.test(masked.slice(cursor))) {
      const open = masked.indexOf('[', cursor);
      let depth = 1;
      let end = open + 1;
      for (; end < masked.length && depth; end++) {
        if (masked[end] === '[') depth++;
        else if (masked[end] === ']') depth--;
      }
      group.push(masked.slice(open + 1, end - 1).trim());
      cursor = end;
      while (/\s/.test(masked[cursor] ?? '') && cursor < masked.length) cursor++;
    }
    attributes.lastIndex = cursor;
    if (!group.some(attr => {
      const cfg = /^cfg\s*\(([\s\S]*)\)$/.exec(attr);
      return cfg ? requiresTest(cfg[1]) : /^(?:\w+::)*test(?:\s*\(|$)/.test(attr);
    })) continue;
    if (/^(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+(?:r#)?\w+\s*;/.test(masked.slice(cursor))) continue;
    const end = itemEnd(masked, cursor);
    const first = source.slice(0, start).split('\n').length;
    const last = source.slice(0, end).split('\n').length;
    const newline = source.indexOf('\n', cursor);
    const headerEnd = newline === -1 ? source.length : newline;
    if (comments.some(comment => comment.start >= start && comment.start < headerEnd && /^\/\/[^\S\r\n]*allow-test-in-production:[^\S\r\n]*\S/.test(comment.text))) { attributes.lastIndex = end; continue; }
    const changed = [...addedLines].find(line => line >= first && line <= last);
    if (changed !== undefined) failures.push(`${file}:${changed}: rust-test-layout: move test bodies and test-only fixtures into a sibling *_tests.rs; keep the sibling module declaration in the owner. A shared helper needs // allow-test-in-production: <reason> on its attribute or declaration.`);
    // One finding for the owning item, rather than one per nested test.
    attributes.lastIndex = end;
  }
  return failures;
}
