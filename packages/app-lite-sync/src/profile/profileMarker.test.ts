import { access, mkdir, mkdtemp, readFile, readdir, rename, rm, symlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { claimProfile, PROFILE_MARKER_CONTENT } from './profileMarker';
import { PROFILE_DIRECTORY_NAME, validateProfilePath, type ValidatedProfilePaths } from './pathPolicy';

const tempParents = new Set<string>();

async function scaffold(options: { marker?: string; unknown?: string; nonEmpty?: boolean } = {}): Promise<ValidatedProfilePaths> {
	const parent = await mkdtemp(join(tmpdir(), 'joplin-lite-marker-'));
	tempParents.add(parent);
	const root = join(parent, PROFILE_DIRECTORY_NAME);
	await mkdir(root);
	for (const name of ['resources', 'indexes', 'logs']) await mkdir(join(root, name));
	if (options.nonEmpty) await writeFile(join(root, 'resources', 'fixture.txt'), 'fixture');
	if (options.unknown) await writeFile(join(root, options.unknown), 'unknown');
	if (options.marker !== undefined) await writeFile(join(root, '.joplin-lite-profile.json'), options.marker);
	return validateProfilePath(root);
}

const NOT_OWNED = { code: 'PROFILE_NOT_OWNED', message: '资料库不属于 Joplin Lite' };

describe('claimProfile', () => {
	afterEach(async () => {
		for (const parent of tempParents) await rm(parent, { recursive: true, force: true });
		tempParents.clear();
	});

	test('claims exactly the empty Rust scaffold and writes the fixed marker', async () => {
		const paths = await scaffold();
		await claimProfile(paths);
		await expect(readFile(paths.marker, 'utf8')).resolves.toBe(PROFILE_MARKER_CONTENT);
	});

	test('accepts an already-owned exact marker without rewriting it', async () => {
		const paths = await scaffold({ marker: PROFILE_MARKER_CONTENT });
		await claimProfile(paths);
		await expect(readFile(paths.marker, 'utf8')).resolves.toBe(PROFILE_MARKER_CONTENT);
	});

	test.each([
		'{"owner":"other","formatVersion":1}',
		'{"owner":"com.kevinhao.joplin-lite","formatVersion":2}',
		'{"owner":"com.kevinhao.joplin-lite"',
	])('rejects a marker that is not exact: %s', async marker => {
		const paths = await scaffold({ marker });
		await expect(claimProfile(paths)).rejects.toMatchObject(NOT_OWNED);
	});

	test.each([
		['unknown entry', { unknown: 'unexpected.txt' }],
		['non-empty scaffold', { nonEmpty: true }],
	])('rejects %s without writing a marker', async (_label, options) => {
		const paths = await scaffold(options);
		await expect(claimProfile(paths)).rejects.toMatchObject(NOT_OWNED);
		await expect(access(paths.marker)).rejects.toThrow();
	});

	test('rejects a markerless database instead of adopting it', async () => {
		const paths = await scaffold();
		await writeFile(paths.database, 'not a database');
		await expect(claimProfile(paths)).rejects.toMatchObject(NOT_OWNED);
	});

	test('rejects a fully empty root and a nonexistent root', async () => {
		const parent = await mkdtemp(join(tmpdir(), 'joplin-lite-marker-empty-'));
		tempParents.add(parent);
		const empty = join(parent, PROFILE_DIRECTORY_NAME);
		await mkdir(empty);
		const emptyPaths = await validateProfilePath(empty);
		await expect(claimProfile(emptyPaths)).rejects.toMatchObject(NOT_OWNED);
		const paths = await scaffold();
		const missing = join(parent, 'missing', PROFILE_DIRECTORY_NAME);
		await expect(claimProfile({ ...paths, root: missing, marker: join(missing, '.joplin-lite-profile.json') })).rejects.toMatchObject({ code: 'PROFILE_INVALID', message: '资料库路径无效' });
	});

	test('rejects a marker symlink and leaves the target untouched', async () => {
		const paths = await scaffold();
		const target = join(paths.root, 'marker-target');
		await writeFile(target, 'secret-marker');
		await symlink(target, paths.marker);
		await expect(claimProfile(paths)).rejects.toMatchObject({ code: 'PROFILE_INVALID', message: '资料库路径无效' });
		await expect(readFile(target, 'utf8')).resolves.toBe('secret-marker');
	});

	test('rejects a deterministic half-written marker without overwriting it', async () => {
		const paths = await scaffold({ marker: '' });
		await expect(claimProfile(paths)).rejects.toMatchObject(NOT_OWNED);
		await expect(readFile(paths.marker, 'utf8')).resolves.toBe('');
	});

	test('racing claims never overwrite the marker', async () => {
		const paths = await scaffold();
		const results = await Promise.allSettled([claimProfile(paths), claimProfile(paths)]);
		expect(results.some(result => result.status === 'fulfilled')).toBe(true);
		for (const result of results) {
			if (result.status === 'rejected') expect(result.reason).toMatchObject(NOT_OWNED);
		}
		await expect(readFile(paths.marker, 'utf8')).resolves.toBe(PROFILE_MARKER_CONTENT);
	});

	test('does not mutate rejected scaffold contents', async () => {
		const paths = await scaffold({ unknown: 'unknown.txt' });
		const before = await readdir(paths.root);
		await expect(claimProfile(paths)).rejects.toMatchObject(NOT_OWNED);
		expect(await readdir(paths.root)).toEqual(before);
	});

	test('rejects an old validated object after root replacement and writes no new marker', async () => {
		const paths = await scaffold();
		const oldRoot = `${paths.root}.old`;
		await rename(paths.root, oldRoot);
		await mkdir(paths.root);
		for (const name of ['resources', 'indexes', 'logs']) await mkdir(join(paths.root, name));
		await expect(claimProfile(paths)).rejects.toMatchObject({ code: 'PROFILE_INVALID', message: '资料库路径无效' });
		await expect(access(join(paths.root, '.joplin-lite-profile.json'))).rejects.toThrow();
	});

	test('does not trust forged child paths and never writes outside the validated root', async () => {
		const paths = await scaffold();
		const outside = join(paths.root, '..', 'outside-marker');
		const forged = { ...paths, marker: outside } as ValidatedProfilePaths;
		await expect(claimProfile(forged)).rejects.toMatchObject({ code: 'PROFILE_INVALID' });
		await expect(access(outside)).rejects.toThrow();
	});

	test('freezes the validated facade and never trusts forged child paths', async () => {
		const paths = await scaffold();
		const outside = join(paths.root, '..', 'mutated-marker');
		expect(Object.isFrozen(paths)).toBe(true);
		const forged = { ...paths, marker: outside } as ValidatedProfilePaths;
		await expect(claimProfile(forged)).rejects.toMatchObject({ code: 'PROFILE_INVALID' });
		await expect(access(outside)).rejects.toThrow();
		await claimProfile(paths);
		await expect(readFile(join(paths.root, '.joplin-lite-profile.json'), 'utf8')).resolves.toBe(PROFILE_MARKER_CONTENT);
	});
});
