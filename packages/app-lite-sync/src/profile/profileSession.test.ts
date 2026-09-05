import { access, mkdir, mkdtemp, readFile, rm, writeFile, unlink } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { ProfileSession } from './profileSession';
import { PROFILE_DIRECTORY_NAME } from './pathPolicy';
import { PROFILE_MARKER_CONTENT } from './profileMarker';

type FakeLease = { fd: 198; close: jest.Mock<void, []> };
type FakeRuntime = {
	schemaVersion: number;
	flush: jest.Mock<Promise<void>, []>;
	close: jest.Mock<Promise<void>, []>;
};

async function scaffold(nonEmpty = false): Promise<string> {
	const parent = await mkdtemp(join(tmpdir(), 'joplin-lite-session-'));
	const root = join(parent, PROFILE_DIRECTORY_NAME);
	await mkdir(root);
	await Promise.all(['resources', 'indexes', 'logs'].map(name => mkdir(join(root, name))));
	if (nonEmpty) await writeFile(join(root, 'unexpected.txt'), 'fixture');
	return parent;
}

describe('ProfileSession', () => {
	let parents: string[] = [];

	afterEach(async () => {
		await Promise.all(parents.map(parent => rm(parent, { recursive: true, force: true })));
		parents = [];
	});

	function fakes(options: { openError?: Error } = {}) {
		const lease: FakeLease = { fd: 198, close: jest.fn() };
		const runtime: FakeRuntime = {
			schemaVersion: 42,
			flush: jest.fn(async (): Promise<void> => undefined),
			close: jest.fn(async (): Promise<void> => undefined),
		};
		return {
			lease,
			runtime,
			session: new ProfileSession({
				verifyLease: () => lease,
				runtimeFactory: async () => {
					if (options.openError) throw options.openError;
					return runtime;
				},
			}),
		};
	}

	test('opens a claimed profile and closes runtime before the inherited lease', async () => {
		const parent = await scaffold();
		parents.push(parent);
		const { session, lease, runtime } = fakes();

		expect(session.status()).toEqual({ state: 'closed', formatVersion: 1 });
		await expect(session.open(join(parent, PROFILE_DIRECTORY_NAME))).resolves.toEqual({ state: 'open', schemaVersion: 42, formatVersion: 1 });
		await expect(readFile(join(parent, PROFILE_DIRECTORY_NAME, '.joplin-lite-profile.json'), 'utf8')).resolves.toBe(PROFILE_MARKER_CONTENT);
		await expect(session.open(join(parent, PROFILE_DIRECTORY_NAME))).rejects.toMatchObject({ code: 'PROFILE_ALREADY_OPEN' });
		await session.close();
		expect(runtime.close).toHaveBeenCalledTimes(1);
		expect(lease.close).toHaveBeenCalledTimes(1);
		expect(session.status()).toEqual({ state: 'closed', formatVersion: 1 });
	});

	test('keeps invalid and not-owned failures recoverable while refusing to touch invalid paths', async () => {
		const { session } = fakes();
		await expect(session.open('/tmp/not-a-joplin-profile')).rejects.toMatchObject({ code: 'PROFILE_INVALID' });
		expect(session.status()).toEqual({ state: 'closed', formatVersion: 1 });
	});

	test('retains the inherited lease across a recoverable ownership failure for retry', async () => {
		const parent = await scaffold(true);
		parents.push(parent);
		const { session, lease } = fakes();

		await expect(session.open(join(parent, PROFILE_DIRECTORY_NAME))).rejects.toMatchObject({ code: 'PROFILE_NOT_OWNED' });
		await unlink(join(parent, PROFILE_DIRECTORY_NAME, 'unexpected.txt'));
		await expect(session.open(join(parent, PROFILE_DIRECTORY_NAME))).resolves.toMatchObject({ state: 'open', schemaVersion: 42 });
		await session.close();
		expect(lease.close).toHaveBeenCalledTimes(1);
	});

	test('makes an inherited-lease failure terminal before claiming or opening a database', async () => {
		const parent = await scaffold();
		parents.push(parent);
		const runtime = { schemaVersion: 42, flush: jest.fn(), close: jest.fn() };
		const session = new ProfileSession({
			verifyLease: () => { throw Object.assign(new Error('fixed'), { code: 'PROFILE_LOCK_REQUIRED' }); },
			runtimeFactory: () => runtime as never,
		});

		await expect(session.open(join(parent, PROFILE_DIRECTORY_NAME))).rejects.toMatchObject({ code: 'PROFILE_LOCK_REQUIRED' });
		expect(runtime.close).not.toHaveBeenCalled();
		await expect(access(join(parent, PROFILE_DIRECTORY_NAME, '.joplin-lite-profile.json'))).rejects.toThrow();
	});

	test('cleans a failed runtime and lease and reports a fixed terminal error', async () => {
		const parent = await scaffold();
		parents.push(parent);
		const error = Object.assign(new Error('secret sqlite path'), { code: 'SQLITE_SECRET' });
		const { session, lease, runtime } = fakes({ openError: error });

		await expect(session.open(join(parent, PROFILE_DIRECTORY_NAME))).rejects.toMatchObject({ code: 'PROFILE_OPEN_FAILED', message: '无法打开资料库' });
		expect(runtime.close).not.toHaveBeenCalled();
		expect(lease.close).toHaveBeenCalledTimes(1);
	});
});
