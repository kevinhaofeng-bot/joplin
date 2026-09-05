const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

function validateRelativeBundlePath(value) {
  if (typeof value !== 'string' || value.length === 0 || path.isAbsolute(value)) return false;
  return !value.split(/[\\/]+/).some((part) => part === '..' || part === '');
}

function assertRegularFile(file, label) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile()) throw new Error(`${label} must be a regular file`);
  return stat;
}

function resolveRegularFile(file, label) {
  const resolved = fs.realpathSync(file);
  assertRegularFile(resolved, label);
  return resolved;
}

function resolvePackage(specifier, searchPaths) {
  const packageJson = require.resolve(`${specifier}/package.json`, { paths: searchPaths });
  const packageRoot = path.dirname(packageJson);
  assertRegularFile(packageJson, `${specifier} package manifest`);
  return packageRoot;
}

function copyPackageTree(specifier, sidecarRoot, stagingRoot, copied = new Set(), searchPaths = [sidecarRoot]) {
  if (copied.has(specifier)) return;
  const source = resolvePackage(specifier, searchPaths);
  const packageJson = JSON.parse(fs.readFileSync(path.join(source, 'package.json'), 'utf8'));
  copied.add(specifier);
  const destination = path.join(stagingRoot, 'node_modules', specifier);
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  fs.cpSync(source, destination, { recursive: true, dereference: true });
  const dependencies = { ...packageJson.dependencies, ...packageJson.optionalDependencies };
  for (const dependency of Object.keys(dependencies)) {
    try {
      copyPackageTree(dependency, sidecarRoot, stagingRoot, copied, [source, sidecarRoot]);
    } catch (error) {
      if (!packageJson.optionalDependencies || !(dependency in packageJson.optionalDependencies)) throw error;
    }
  }
}

function findNativeAddon(packageRoot, name) {
  const found = [];
  const visit = (directory) => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const entryPath = path.join(directory, entry.name);
      if (entry.isSymbolicLink()) throw new Error(`symlink in native package: ${entryPath}`);
      if (entry.isDirectory()) visit(entryPath);
      else if (entry.isFile() && entry.name === name) found.push(entryPath);
    }
  };
  visit(packageRoot);
  if (found.length !== 1) throw new Error(`expected one ${name} native addon, found ${found.length}`);
  return found[0];
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], ...options });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed: ${result.stderr || result.stdout}`);
  return result.stdout.trim();
}

function main() {
  if (process.platform !== 'darwin' || process.arch !== 'arm64') {
    throw new Error('release sidecar bundle requires macOS arm64');
  }
  const sidecarRoot = path.resolve(__dirname, '..');
  const repoRoot = path.resolve(sidecarRoot, '../..');
  const appRoot = path.join(repoRoot, 'packages/app-lite');
  const stagingRoot = path.join(appRoot, 'src-tauri/resources/sidecar');
  const nodeRuntime = process.env.JOPLIN_LITE_NODE_RUNTIME;
  if (!nodeRuntime) throw new Error('JOPLIN_LITE_NODE_RUNTIME must point to an explicit arm64 Node runtime');
  if (!process.versions.bun) throw new Error('run this script with the pinned Bun runtime');
  const bunRuntime = resolveRegularFile(process.execPath, 'Bun runtime');
  const nodeRuntimePath = resolveRegularFile(nodeRuntime, 'Node runtime');
  const nodeFile = run('/usr/bin/file', ['-b', nodeRuntimePath]);
  if (!/Mach-O.*arm64/.test(nodeFile)) throw new Error(`Node runtime is not arm64: ${nodeFile}`);

  const manifest = {
    formatVersion: 1,
    platform: 'darwin',
    arch: 'arm64',
    nodePath: 'bin/node',
    entryPath: 'sidecar.cjs',
    currentDir: '.',
    nodeVersion: run(nodeRuntimePath, ['--version']),
    bunVersion: Bun.version,
  };
  if (!validateRelativeBundlePath(manifest.nodePath) || !validateRelativeBundlePath(manifest.entryPath)) {
    throw new Error('invalid generated bundle manifest');
  }
  fs.rmSync(stagingRoot, { recursive: true, force: true });
  fs.mkdirSync(path.join(stagingRoot, 'bin'), { recursive: true });

  const bundleOutput = path.join(stagingRoot, manifest.entryPath);
  const buildArgs = [
    'build', path.join(sidecarRoot, 'src/main.ts'), '--target=node', '--format=cjs',
    '--external', 'sqlite3', '--external', 'keytar', '--external', 'electron',
    '--external', '@joplin/utils/Logger', '--outfile', bundleOutput,
  ];
  run(bunRuntime, buildArgs, { cwd: repoRoot });
  assertRegularFile(bundleOutput, 'sidecar bundle');
  fs.copyFileSync(nodeRuntimePath, path.join(stagingRoot, manifest.nodePath));
  fs.chmodSync(path.join(stagingRoot, manifest.nodePath), 0o755);

  const packageSpecs = [
    ['@joplin/utils', '@joplin/utils'],
    ['moment', 'moment'],
    ['async-mutex', 'async-mutex'],
    ['sprintf-js', 'sprintf-js'],
    ['sqlite3', 'sqlite3'],
    ['keytar', 'keytar'],
  ];
  for (const [specifier, target] of packageSpecs) {
    const source = resolvePackage(specifier, [sidecarRoot]);
    const destination = path.join(stagingRoot, 'node_modules', target);
    fs.mkdirSync(path.dirname(destination), { recursive: true });
    fs.cpSync(source, destination, { recursive: true, dereference: true });
  }
  const copiedPackages = new Set();
  copyPackageTree('sqlite3', sidecarRoot, stagingRoot, copiedPackages);
  copyPackageTree('keytar', sidecarRoot, stagingRoot, copiedPackages);
  const sqliteAddon = findNativeAddon(path.join(stagingRoot, 'node_modules/sqlite3'), 'node_sqlite3.node');
  const keytarAddon = findNativeAddon(path.join(stagingRoot, 'node_modules/keytar'), 'keytar.node');
  for (const addon of [sqliteAddon, keytarAddon]) {
    const description = run('/usr/bin/file', ['-b', addon]);
    if (!/Mach-O.*arm64/.test(description)) throw new Error(`native addon is not arm64: ${description}`);
  }
  fs.writeFileSync(path.join(stagingRoot, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`, { mode: 0o644 });
  const metadata = fs.statSync(stagingRoot);
  if (!metadata.isDirectory()) throw new Error('sidecar staging directory missing');
  process.stdout.write(`${JSON.stringify({ stagingRoot, manifest, bunRuntime, nodeRuntime })}\n`);
}

module.exports = { validateRelativeBundlePath };

if (require.main === module) main();
