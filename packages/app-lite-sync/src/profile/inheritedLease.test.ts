import { closeSync, mkdirSync, mkdtempSync, openSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { validateProfilePath } from './pathPolicy';
import { verifyInheritedLease } from './inheritedLease';

const LEASE_NAME = '.com.kevinhao.joplin-lite.canonical.lock';
const originalEnv = process.env.JOPLIN_LITE_PROFILE_LEASE_FD;

describe('verifyInheritedLease', () => {
	let parent: string;
	let fd: number | undefined;

	beforeEach(() => {
		parent = mkdtempSync(join(tmpdir(), 'joplin-lite-lease-'));
		const root = join(parent, 'com.kevinhao.joplin-lite');
		mkdirSync(root);
		for (const name of ['resources', 'indexes', 'logs']) mkdirSync(join(root, name));
	});
	afterEach(() => {
		if (fd !== undefined) closeSync(fd);
		fd = undefined;
		if (originalEnv === undefined) delete process.env.JOPLIN_LITE_PROFILE_LEASE_FD;
		else process.env.JOPLIN_LITE_PROFILE_LEASE_FD = originalEnv;
		rmSync(parent, { recursive: true, force: true });
	});

	test('rejects a missing inherited descriptor', async () => {
		delete process.env.JOPLIN_LITE_PROFILE_LEASE_FD;
		const paths = await validateProfilePath(join(parent, 'com.kevinhao.joplin-lite'));
		expect(() => verifyInheritedLease(paths)).toThrow('资料库写入租约无效');
	});

	test('rejects a descriptor whose identity is not the sibling lease', async () => {
		const paths = await validateProfilePath(join(parent, 'com.kevinhao.joplin-lite'));
		const unrelated = join(parent, 'unrelated');
		writeFileSync(unrelated, 'fixture');
		fd = openSync(unrelated, 'r');
		process.env.JOPLIN_LITE_PROFILE_LEASE_FD = '198';
		const result = runVerifier(paths.root, fd);
		expect(result.status).not.toBe(0);
	});

	test('accepts the inherited descriptor matching the sibling lease', async () => {
		const paths = await validateProfilePath(join(parent, 'com.kevinhao.joplin-lite'));
		const lock = join(parent, LEASE_NAME);
		fd = openSync(lock, 'a+', 0o600);
		process.env.JOPLIN_LITE_PROFILE_LEASE_FD = '198';
		const result = runVerifier(paths.root, fd);
		expect(result.status).toBe(0);
	});
});

function runVerifier(root: string, inheritedFd: number): ReturnType<typeof spawnSync> {
	const stdio = Array.from({ length: 199 }, () => 'ignore') as any[];
	stdio[198] = inheritedFd;
	const script = "const { verifyInheritedLease } = require('./src/profile/inheritedLease'); const { validateProfilePath } = require('./src/profile/pathPolicy'); validateProfilePath(process.argv[1]).then(paths => { const lease = verifyInheritedLease(paths); lease.close(); }).catch(() => { process.exitCode = 1; });";
	return spawnSync(process.execPath, ['-r', 'ts-node/register/transpile-only', '-e', script, root], {
		cwd: join(__dirname, '..', '..'),
		env: { ...process.env, JOPLIN_LITE_PROFILE_LEASE_FD: '198' },
		stdio,
	});
}
