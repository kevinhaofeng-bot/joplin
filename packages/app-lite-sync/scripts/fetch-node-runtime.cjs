const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const PINNED_NODE_VERSION = 'v23.11.0';
const PINNED_NODE_ARCHIVE_SHA256 = '635990b46610238e3c008cd01480c296e0c2bfe7ec59ea9a8cd789d5ac621bb0';
const PINNED_NODE_SHA256 = 'ead5889f15f4e17b37f941c7d9e92b624396786bd74d8155ef470f6381f032c5';
const ARCHIVE = `node-${PINNED_NODE_VERSION}-darwin-arm64.tar.gz`;
const BASE_URL = `https://nodejs.org/dist/${PINNED_NODE_VERSION}`;

function run(command, args) {
  const result = spawnSync(command, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed: ${result.stderr || result.stdout}`);
  return result.stdout.trim();
}

function fetchOfficialRuntime(outputPath) {
  if (process.platform !== 'darwin' || process.arch !== 'arm64') throw new Error('official Node runtime requires macOS arm64');
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'joplin-node-runtime-'));
  try {
    const archivePath = path.join(temp, ARCHIVE);
    run('curl', ['-fsSL', `${BASE_URL}/${ARCHIVE}`, '-o', archivePath]);
    const sums = run('curl', ['-fsSL', `${BASE_URL}/SHASUMS256.txt`]);
    const expected = sums.split('\n').find(line => line.endsWith(`  ${ARCHIVE}`))?.split(/\s+/)[0];
    if (expected !== PINNED_NODE_ARCHIVE_SHA256) throw new Error('official Node SHASUMS256 pin changed');
    const actual = crypto.createHash('sha256').update(fs.readFileSync(archivePath)).digest('hex');
    if (actual !== expected) throw new Error('official Node archive checksum mismatch');
    run('tar', ['-xzf', archivePath, '-C', temp]);
    const extracted = path.join(temp, `node-${PINNED_NODE_VERSION}-darwin-arm64`, 'bin/node');
    const stat = fs.lstatSync(extracted);
    if (!stat.isFile()) throw new Error('official Node archive did not contain a regular runtime');
    const runtimeSha256 = crypto.createHash('sha256').update(fs.readFileSync(extracted)).digest('hex');
    if (runtimeSha256 !== PINNED_NODE_SHA256) throw new Error('official Node runtime checksum mismatch');
    fs.mkdirSync(path.dirname(outputPath), { recursive: true });
    fs.copyFileSync(extracted, outputPath);
    fs.chmodSync(outputPath, 0o755);
    fs.writeFileSync(`${outputPath}.sha256`, `${runtimeSha256}\n`);
    process.stdout.write(`${JSON.stringify({ outputPath, version: PINNED_NODE_VERSION, sha256: runtimeSha256 })}\n`);
  } finally {
    fs.rmSync(temp, { recursive: true, force: true });
  }
}

module.exports = { PINNED_NODE_SHA256, PINNED_NODE_VERSION, fetchOfficialRuntime };

if (require.main === module) {
  const outputPath = process.argv[2];
  if (!outputPath) throw new Error('usage: fetch-node-runtime.cjs OUTPUT_PATH');
  fetchOfficialRuntime(path.resolve(outputPath));
}
