import { readFileSync } from 'node:fs';
import { join } from 'node:path';

describe('clean sidecar verification contract', () => {
	test('builds the required upstream outputs before running sidecar checks', () => {
		const packageJson = JSON.parse(readFileSync(join(__dirname, '..', 'package.json'), 'utf8')) as {
			scripts?: Record<string, string>;
		};
		const script = packageJson.scripts?.['verify:clean'];

		expect(script).toBe([
			'yarn workspace @joplin/fork-htmlparser2 build',
			'yarn workspace @joplin/utils build',
			'yarn workspace @joplin/lib tsc',
			'yarn test',
			'yarn tsc',
		].join(' && '));
		expect(script).not.toMatch(/workspace @joplin\/app-lite-sync\s+(test|tsc)/);
	});
});
