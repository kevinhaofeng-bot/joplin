#!/usr/bin/env node
// Generates the compact Rust-side projection of Joplin's checked-in MIME table.
// Run from the repository root after updating packages/lib/mime-utils-types.ts.

import { readFileSync, writeFileSync } from 'node:fs';

const source = readFileSync('packages/lib/mime-utils-types.ts', 'utf8');
const tableEntries = [
  ...source.matchAll(/\{ t: '([^']+)', e: \[([^\]]*)\] \}/g),
  ...source.matchAll(/mimeTypes\.push\(\{ t: '([^']+)', e: \[([^\]]*)\] \}\)/g),
];
const seen = new Set();
const rows = [];
for (const entry of tableEntries) {
  const mime = entry[1];
  if (seen.has(mime)) continue;
  seen.add(mime);
  const extensions = [...entry[2].matchAll(/'([^']+)'/g)].map(match => match[1]);
  const selected = extensions.find(extension => extension.length === 3) ?? extensions[0];
  if (selected) rows.push(`${mime}\t${selected}`);
}

writeFileSync(
  'packages/app-lite-core/src/joplin_mime_extensions.tsv',
  [
    '# Generated from packages/lib/mime-utils-types.ts.',
    '# Regenerate: node scripts/generate-joplin-mime-extensions.mjs',
    '# Each row is MIME<TAB>Joplin mime-utils.ts toFileExtension output.',
    ...rows,
    '',
  ].join('\n'),
);
