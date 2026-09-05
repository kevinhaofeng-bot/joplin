#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const childProcess = require('node:child_process');

const sidecarRoot = path.resolve(__dirname, '..');
const sidecarNodeModules = path.join(sidecarRoot, 'node_modules');

function packageRoot() {
	const packageJson = require.resolve('sqlite3/package.json', { paths: [sidecarRoot] });
	const resolved = fs.realpathSync(packageJson);
	const root = fs.realpathSync(path.dirname(resolved));
	const relative = path.relative(sidecarNodeModules, root);
	if (!relative || relative.startsWith('..') || path.isAbsolute(relative) || relative.split(path.sep)[0] !== 'sqlite3') {
		throw new Error('sqlite3 is not installed in the sidecar package');
	}
	return root;
}

function probe() {
	try {
		const sqlite3 = require('sqlite3');
		return new Promise(resolve => {
			let database;
			try {
				database = new sqlite3.Database(':memory:');
			} catch {
				resolve(false);
				return;
			}
			database.get('SELECT 1', (error, row) => {
				database.close(closeError => resolve(!error && !closeError && row && row[1] === 1));
			});
		});
	} catch {
		return Promise.resolve(false);
	}
}

function install(packageDir) {
	const preGyp = require.resolve('@mapbox/node-pre-gyp/bin/node-pre-gyp', { paths: [packageDir] });
	const result = childProcess.spawnFileSync(process.execPath, [preGyp, 'install', '--fallback-to-build'], {
		cwd: packageDir,
		stdio: 'inherit',
		env: process.env,
	});
	if (result.error || result.status !== 0) throw result.error || new Error('sqlite3 native bootstrap failed');
}

async function main() {
	const packageDir = packageRoot();
	if (!await probe()) {
		install(packageDir);
		if (!await probe()) throw new Error('sqlite3 memory probe failed after native bootstrap');
	}

}

main().catch(error => {
	process.stderr.write(`sqlite3 bootstrap failed: ${error.message}\n`);
	process.exitCode = 1;
});
