import { readFileSync } from 'node:fs';
import { join } from 'node:path';

describe('clean sidecar verification contract', () => {
	test('builds the required upstream outputs before running sidecar checks', () => {
		const packageJson = JSON.parse(readFileSync(join(__dirname, '..', 'package.json'), 'utf8')) as {
			scripts?: Record<string, string>;
		};
		const script = packageJson.scripts?.['verify:clean'];

		expect(script).toBe([
			'node scripts/bootstrap-sqlite3.cjs',
			'yarn workspace @joplin/fork-htmlparser2 build',
			'yarn workspace @joplin/utils build',
			'yarn workspace @joplin/lib tsc',
			'yarn test',
			'yarn tsc',
		].join(' && '));
		expect(script).not.toMatch(/workspace @joplin\/app-lite-sync\s+(test|tsc)/);
	});

	test('declares sqlite3 as a direct sidecar dependency', () => {
		const packageJson = JSON.parse(readFileSync(join(__dirname, '..', 'package.json'), 'utf8')) as {
			dependencies?: Record<string, string>;
		};
		expect(packageJson.dependencies?.sqlite3).toBe('5.1.6');
	});

	test('keeps sqlite bootstrap scoped to the sidecar package', () => {
		const script = readFileSync(join(__dirname, '..', 'scripts', 'bootstrap-sqlite3.cjs'), 'utf8');
		expect(script).toContain("require.resolve('sqlite3/package.json'");
		expect(script).toContain('--fallback-to-build');
		expect(script).not.toMatch(/yarn\s+(workspace\s+[^\s]+\s+)?rebuild/);
		expect(script).not.toMatch(/shell:\s*true/);
	});
});
