const fs = require('fs');
const path = require('path');

function findFiles(dir, res=[]) {
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  for (const e of entries) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) findFiles(p, res);
    else if (e.isFile() && p.endsWith('.ts')) res.push(p);
  }
  return res;
}

function extractSchemas(file) {
  const text = fs.readFileSync(file, 'utf8');
  const schemas = [];
  const idx = text.indexOf('schema: (z) => ({');
  if (idx === -1) return schemas;
  // crude: find from 'schema: (z) => ({' to the next '})' that closes top
  let start = text.indexOf('schema: (z) => ({', 0);
  while (start !== -1) {
    let i = start + 'schema: (z) => ({'.length;
    let depth = 1;
    while (i < text.length && depth > 0) {
      const ch = text[i];
      if (ch === '{') depth++;
      else if (ch === '}') depth--;
      i++;
    }
    const snippet = text.slice(start, i);
    schemas.push(snippet);
    start = text.indexOf('schema: (z) => ({', i);
  }
  return schemas;
}

function mapType(zExpr) {
  zExpr = zExpr.trim();
  if (zExpr.startsWith('z.string')) return 'String';
  if (zExpr.startsWith('z.number')) return 'i64';
  if (zExpr.startsWith('z.boolean')) return 'bool';
  if (zExpr.startsWith('z.any') || zExpr.startsWith('z.unknown')) return 'serde_json::Value';
  if (zExpr.startsWith('z.array')) return 'Vec<serde_json::Value>';
  if (zExpr.includes('z.object')) return 'serde_json::Value';
  if (zExpr.includes('z.union')) return 'serde_json::Value';
  return 'serde_json::Value';
}

function parseBodyObject(snippet) {
  // find 'body: z.object({' and extract interior until matching '})'
  const m = snippet.match(/body:\s*z\.object\s*\(\s*\{([\s\S]*?)\}\s*\)/m);
  if (!m) return null;
  const body = m[1];
  const lines = body.split(/\n/).map(l=>l.trim()).filter(Boolean);
  const fields = [];
  for (let line of lines) {
    // match `foo: z.string().meta(...),` or `foo: z.number().optional(),`
    const mm = line.match(/^([A-Za-z0-9_]+)\s*:\s*([\s\S]+?),(?:$|\/\/)/);
    if (mm) {
      const key = mm[1];
      const zexpr = mm[2];
      const ty = mapType(zexpr);
      fields.push({key, ty});
    }
  }
  return fields;
}

function generateStruct(name, fields) {
  const lines = [];
  lines.push(`# ${name} — generated from Zod schema`);
  lines.push('```rust');
  lines.push('use serde::{Serialize, Deserialize};');
  lines.push('');
  lines.push('#[derive(Debug, Serialize, Deserialize)]');
  lines.push('#[serde(rename_all = "camelCase")]');
  lines.push(`pub struct ${name} {`);
  for (const f of fields) {
    lines.push(`    pub ${f.key}: ${f.ty},`);
  }
  lines.push('}');
  lines.push('```');
  lines.push('');
  return lines.join('\n');
}

function main() {
  // Absolute path to the cloned Talo repo in workspace
  const repo = path.join('/home/node/.openclaw/workspace', 'git_repos', 'talo-backend', 'src');
  const files = findFiles(repo);
  const out = [];
  for (const f of files) {
    const schemas = extractSchemas(f);
    for (const s of schemas) {
      const fields = parseBodyObject(s);
      if (fields && fields.length) {
        const nm = path.basename(f).replace(/\.ts$/, '') + '_Body';
        out.push(generateStruct(nm, fields));
      }
    }
  }
  const target = path.join('/home/node/.openclaw/workspace', 'git_repos', 'Playzoid-Server', 'docs', 'TALO_API_STRUCTS.md');
  fs.writeFileSync(target, out.join('\n\n') || '// No simple schemas extracted\n');
  console.log('Wrote', target);
}

main();
